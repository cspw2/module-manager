use std::collections::HashMap;
use std::path::Path;
use anyhow::{anyhow, Context, Result};
use cspw_module_sdk::{
    ApiResponse, BinaryFile, CreateModuleFn, DestroyModuleFn, Module, ModuleMetadata,
};
use libloading::Library;

#[cfg(test)]
mod tests;

pub struct ModuleHandle {
    // Keep library loaded in memory
    _library: Library,
    instance_ptr: *mut Box<dyn Module>,
    destroy_fn: DestroyModuleFn,
    metadata: ModuleMetadata,
}

// ModuleHandle is safe to send across threads if the module is Send + Sync
unsafe impl Send for ModuleHandle {}
unsafe impl Sync for ModuleHandle {}

impl ModuleHandle {
    pub fn metadata(&self) -> &ModuleMetadata {
        &self.metadata
    }

    pub fn execute(
        &mut self,
        api_name: &str,
        file: &mut BinaryFile,
        args: &serde_json::Value,
    ) -> Result<ApiResponse> {
        unsafe {
            let module: &mut Box<dyn Module> = &mut *self.instance_ptr;
            module.execute(api_name, file, args)
        }
    }
}

impl Drop for ModuleHandle {
    fn drop(&mut self) {
        if !self.instance_ptr.is_null() {
            unsafe {
                (self.destroy_fn)(self.instance_ptr);
            }
            self.instance_ptr = std::ptr::null_mut();
        }
    }
}

#[derive(Default)]
pub struct ModuleManager {
    modules: HashMap<String, ModuleHandle>,
}

impl ModuleManager {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    /// Load a compiled dynamic library (.dll / .so / .dylib)
    pub fn load_module<P: AsRef<Path>>(&mut self, path: P) -> Result<&ModuleMetadata> {
        let path_ref = path.as_ref();
        let library = unsafe {
            Library::new(path_ref)
                .with_context(|| format!("Failed to load dynamic library at {:?}", path_ref))?
        };

        let create_fn = unsafe {
            let symbol = library
                .get::<CreateModuleFn>(b"cspw_module_create\0")
                .context("Symbol 'cspw_module_create' not found in module")?;
            *symbol
        };

        let destroy_fn = unsafe {
            let symbol = library
                .get::<DestroyModuleFn>(b"cspw_module_destroy\0")
                .context("Symbol 'cspw_module_destroy' not found in module")?;
            *symbol
        };

        let instance_ptr = unsafe { (create_fn)() };
        if instance_ptr.is_null() {
            return Err(anyhow!("Module creation returned a null pointer"));
        }

        let metadata = unsafe {
            let module: &Box<dyn Module> = &*instance_ptr;
            module.metadata()
        };

        let name = metadata.name.clone();
        let handle = ModuleHandle {
            _library: library,
            instance_ptr,
            destroy_fn,
            metadata,
        };

        self.modules.insert(name.clone(), handle);
        Ok(&self.modules.get(&name).unwrap().metadata)
    }

    /// Scan a directory and load all modules
    pub fn load_directory<P: AsRef<Path>>(&mut self, dir: P) -> Result<usize> {
        let mut count = 0;
        let entries = std::fs::read_dir(dir.as_ref())?;

        for entry in entries.flatten() {
            let path = entry.path();
            let is_dynlib = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "dll" || ext == "so" || ext == "dylib")
                .unwrap_or(false);

            if is_dynlib && self.load_module(&path).is_ok() {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn list_modules(&self) -> Vec<&ModuleMetadata> {
        self.modules.values().map(|h| h.metadata()).collect()
    }

    pub fn get_metadata(&self, name: &str) -> Option<&ModuleMetadata> {
        self.modules.get(name).map(|h| h.metadata())
    }

    pub fn execute(
        &mut self,
        module_name: &str,
        api_name: &str,
        file: &mut BinaryFile,
        args: &serde_json::Value,
    ) -> Result<ApiResponse> {
        let handle = self
            .modules
            .get_mut(module_name)
            .ok_or_else(|| anyhow!("Module '{}' not loaded", module_name))?;

        handle.execute(api_name, file, args)
    }
}