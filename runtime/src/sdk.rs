#![allow(clippy::missing_safety_doc)]

use std::os::raw::c_char;
use std::str::FromStr;

use crate::*;

/// Create a new context
#[no_mangle]
pub unsafe extern "C" fn extism_context_new() -> *mut Context {
    trace!("Creating new Context");
    Box::into_raw(Box::new(Context::new()))
}

/// Free a context
#[no_mangle]
pub unsafe extern "C" fn extism_context_free(ctx: *mut Context) {
    trace!("Freeing context");
    if ctx.is_null() {
        return;
    }
    drop(Box::from_raw(ctx))
}

/// Create a new plugin
///
/// `wasm`: is a WASM module (wat or wasm) or a JSON encoded manifest
/// `wasm_size`: the length of the `wasm` parameter
/// `with_wasi`: enables/disables WASI
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_new(
    ctx: *mut Context,
    wasm: *const u8,
    wasm_size: Size,
    with_wasi: bool,
) -> PluginIndex {
    trace!("Call to extism_plugin_new with wasm pointer {:?}", wasm);
    let ctx = &mut *ctx;
    let data = std::slice::from_raw_parts(wasm, wasm_size as usize);
    ctx.new_plugin(data, with_wasi)
}

/// Update a plugin, keeping the existing ID
///
/// Similar to `extism_plugin_new` but takes an `index` argument to specify
/// which plugin to update
///
/// Memory for this plugin will be reset upon update
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_update(
    ctx: *mut Context,
    index: PluginIndex,
    wasm: *const u8,
    wasm_size: Size,
    with_wasi: bool,
) -> bool {
    trace!("Call to extism_plugin_update with wasm pointer {:?}", wasm);
    let ctx = &mut *ctx;

    let data = std::slice::from_raw_parts(wasm, wasm_size as usize);
    let plugin = match Plugin::new(data, with_wasi) {
        Ok(x) => x,
        Err(e) => {
            error!("Error creating Plugin: {:?}", e);
            ctx.set_error(e);
            return false;
        }
    };

    if !ctx.plugins.contains_key(&index) {
        ctx.set_error("Plugin index does not exist");
        return false;
    }

    ctx.plugins.insert(index, plugin);

    info!("Plugin updated: {index}");
    true
}

/// Remove a plugin from the registry and free associated memory
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_free(ctx: *mut Context, plugin: PluginIndex) {
    if plugin < 0 || ctx.is_null() {
        return;
    }

    trace!("Freeing plugin {plugin}");

    let ctx = &mut *ctx;
    ctx.remove(plugin);
}

/// Remove all plugins from the registry
#[no_mangle]
pub unsafe extern "C" fn extism_context_reset(ctx: *mut Context) {
    let ctx = &mut *ctx;

    trace!(
        "Resetting context, plugins cleared: {:?}",
        ctx.plugins.keys().collect::<Vec<&i32>>()
    );

    ctx.plugins.clear();
}

/// Update plugin config values, this will merge with the existing values
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_config(
    ctx: *mut Context,
    plugin: PluginIndex,
    json: *const u8,
    json_size: Size,
) -> bool {
    let ctx = &mut *ctx;
    let mut plugin = match PluginRef::new(ctx, plugin, true) {
        None => return false,
        Some(p) => p,
    };

    trace!(
        "Call to extism_plugin_config for {} with json pointer {:?}",
        plugin.id,
        json
    );

    let data = std::slice::from_raw_parts(json, json_size as usize);
    let json: std::collections::BTreeMap<String, Option<String>> =
        match serde_json::from_slice(data) {
            Ok(x) => x,
            Err(e) => {
                return plugin.as_mut().error(e, false);
            }
        };

    let plugin = plugin.as_mut();

    let wasi = &mut plugin.memory.store.data_mut().wasi;
    let config = &mut plugin.manifest.as_mut().config;
    for (k, v) in json.into_iter() {
        match v {
            Some(v) => {
                trace!("Config, adding {k}");
                if let Some(Wasi { ctx, .. }) = wasi {
                    let _ = ctx.push_env(&k, &v);
                }
                config.insert(k, v);
            }
            None => {
                trace!("Config, removing {k}");
                if let Some(Wasi { ctx, .. }) = wasi {
                    let _ = ctx.push_env(&k, "");
                }
                config.remove(&k);
            }
        }
    }

    true
}

/// Returns true if `func_name` exists
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_function_exists(
    ctx: *mut Context,
    plugin: PluginIndex,
    func_name: *const c_char,
) -> bool {
    let ctx = &mut *ctx;
    let mut plugin = match PluginRef::new(ctx, plugin, true) {
        None => return false,
        Some(p) => p,
    };

    let name = std::ffi::CStr::from_ptr(func_name);
    trace!("Call to extism_plugin_function_exists for: {:?}", name);

    let name = match name.to_str() {
        Ok(x) => x,
        Err(e) => {
            return plugin.as_mut().error(e, false);
        }
    };

    plugin.as_mut().get_func(name).is_some()
}

/// Call a function
///
/// `func_name`: is the function to call
/// `data`: is the input data
/// `data_len`: is the length of `data`
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_call(
    ctx: *mut Context,
    plugin_id: PluginIndex,
    func_name: *const c_char,
    data: *const u8,
    data_len: Size,
) -> i32 {
    let ctx = &mut *ctx;

    // Get a `PluginRef` and call `init` to set up the plugin input and memory, this is only
    // needed before a new call
    let mut plugin_ref = match PluginRef::new(ctx, plugin_id, true) {
        None => return -1,
        Some(p) => p.init(data, data_len as usize),
    };

    // Find function
    let name = std::ffi::CStr::from_ptr(func_name);
    let name = match name.to_str() {
        Ok(name) => name,
        Err(e) => return plugin_ref.as_ref().error(e, -1),
    };

    debug!("Calling function: {name} in plugin {plugin_id}");

    let func = match plugin_ref.as_mut().get_func(name) {
        Some(x) => x,
        None => {
            return plugin_ref
                .as_ref()
                .error(format!("Function not found: {name}"), -1)
        }
    };

    // Check the number of results, reject functions with more than 1 result
    let n_results = func.ty(&plugin_ref.as_ref().memory.store).results().len();
    if n_results > 1 {
        return plugin_ref.as_ref().error(
            format!("Function {name} has {n_results} results, expected 0 or 1"),
            -1,
        );
    }

    // Start timer
    let tx = plugin_ref.epoch_timer_tx.clone();
    if let Err(e) = plugin_ref.as_mut().start_timer(&tx) {
        let id = plugin_ref.as_ref().timer_id;
        return plugin_ref.as_ref().error(
            format!("Unable to start timeout manager for {id}: {e:?}"),
            -1,
        );
    }

    // Call the function
    let mut results = vec![Val::null(); n_results];
    let res = func.call(
        &mut plugin_ref.as_mut().memory.store,
        &[],
        results.as_mut_slice(),
    );

    plugin_ref.as_ref().dump_memory();

    if plugin_ref.as_ref().has_wasi() && name == "_start" {
        plugin_ref.as_mut().should_reinstantiate = true;
    }

    // Stop timer
    if let Err(e) = plugin_ref.as_mut().stop_timer(&tx) {
        let id = plugin_ref.as_ref().timer_id;
        return plugin_ref.as_ref().error(
            format!("Failed to stop timeout manager for {id}: {e:?}"),
            -1,
        );
    }

    match res {
        Ok(()) => (),
        Err(e) => {
            let plugin = plugin_ref.as_ref();
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                trace!("WASI return code: {}", exit.0);
                if exit.0 != 0 {
                    return plugin.error(&e, exit.0);
                }
                return exit.0;
            }

            if e.root_cause().to_string() == "timeout" {
                return plugin.error("timeout", -1);
            }

            error!("Call: {e:?}");
            return plugin.error(e.context("Call failed"), -1);
        }
    };

    // If `results` is empty and the return value wasn't a WASI exit code then
    // the call succeeded
    if results.is_empty() {
        return 0;
    }

    // Return result to caller
    results[0].unwrap_i32()
}

pub fn get_context_error(ctx: &Context) -> *const c_char {
    match &ctx.error {
        Some(e) => e.as_ptr() as *const _,
        None => {
            trace!("Context error is NULL");
            std::ptr::null()
        }
    }
}

/// Get the error associated with a `Context` or `Plugin`, if `plugin` is `-1` then the context
/// error will be returned
#[no_mangle]
pub unsafe extern "C" fn extism_error(ctx: *mut Context, plugin: PluginIndex) -> *const c_char {
    trace!("Call to extism_error for plugin {plugin}");

    let ctx = &mut *ctx;

    if !ctx.plugin_exists(plugin) {
        return get_context_error(ctx);
    }

    let plugin = match PluginRef::new(ctx, plugin, false) {
        None => return std::ptr::null(),
        Some(p) => p,
    };

    let err = plugin.as_ref().last_error.borrow();
    match err.as_ref() {
        Some(e) => e.as_ptr() as *const _,
        None => {
            trace!("Error is NULL");
            std::ptr::null()
        }
    }
}

/// Get the length of a plugin's output data
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_output_length(
    ctx: *mut Context,
    plugin: PluginIndex,
) -> Size {
    trace!("Call to extism_plugin_output_length for plugin {plugin}");

    let ctx = &mut *ctx;
    let plugin = match PluginRef::new(ctx, plugin, true) {
        None => return 0,
        Some(p) => p,
    };

    let len = plugin.as_ref().memory.store.data().output_length as Size;
    trace!("Output length: {len}");
    len
}

/// Get the length of a plugin's output data
#[no_mangle]
pub unsafe extern "C" fn extism_plugin_output_data(
    ctx: *mut Context,
    plugin: PluginIndex,
) -> *const u8 {
    trace!("Call to extism_plugin_output_data for plugin {plugin}");

    let ctx = &mut *ctx;
    let plugin = match PluginRef::new(ctx, plugin, true) {
        None => return std::ptr::null(),
        Some(p) => p,
    };
    let data = plugin.as_ref().memory.store.data();

    plugin
        .as_ref()
        .memory
        .ptr(MemoryBlock::new(data.output_offset, data.output_length))
        .map(|x| x as *const _)
        .unwrap_or(std::ptr::null())
}

/// Set log file and level
#[no_mangle]
pub unsafe extern "C" fn extism_log_file(
    filename: *const c_char,
    log_level: *const c_char,
) -> bool {
    use log::LevelFilter;
    use log4rs::append::console::ConsoleAppender;
    use log4rs::append::file::FileAppender;
    use log4rs::config::{Appender, Config, Logger, Root};
    use log4rs::encode::pattern::PatternEncoder;

    let file = if !filename.is_null() {
        let file = std::ffi::CStr::from_ptr(filename);
        match file.to_str() {
            Ok(x) => x,
            Err(_) => {
                return false;
            }
        }
    } else {
        "stderr"
    };

    let level = if log_level.is_null() {
        "error"
    } else {
        let level = std::ffi::CStr::from_ptr(log_level);
        match level.to_str() {
            Ok(x) => x,
            Err(_) => {
                return false;
            }
        }
    };

    let level = match LevelFilter::from_str(level) {
        Ok(x) => x,
        Err(_) => {
            return false;
        }
    };

    let encoder = Box::new(PatternEncoder::new("{t} {l} {d} - {m}\n"));

    let logfile: Box<dyn log4rs::append::Append> =
        if file == "-" || file == "stdout" || file == "stderr" {
            let target = if file == "-" || file == "stdout" {
                log4rs::append::console::Target::Stdout
            } else {
                log4rs::append::console::Target::Stderr
            };
            let console = ConsoleAppender::builder().target(target).encoder(encoder);
            Box::new(console.build())
        } else {
            match FileAppender::builder().encoder(encoder).build(file) {
                Ok(x) => Box::new(x),
                Err(_) => {
                    return false;
                }
            }
        };

    let config = match Config::builder()
        .appender(Appender::builder().build("logfile", logfile))
        .logger(
            Logger::builder()
                .appender("logfile")
                .build("extism_runtime", level),
        )
        .build(Root::builder().build(LevelFilter::Off))
    {
        Ok(x) => x,
        Err(_) => {
            return false;
        }
    };

    if log4rs::init_config(config).is_err() {
        return false;
    }
    true
}

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Get the Extism version string
#[no_mangle]
pub unsafe extern "C" fn extism_version() -> *const c_char {
    VERSION.as_ptr() as *const _
}
