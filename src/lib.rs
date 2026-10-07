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
    _temp_dir: Option<tempfile::TempDir>,
}

// ModuleHandle is safe to send across threads if the module is Send + Sync
unsafe impl Send for ModuleHandle {}
unsafe impl Sync for ModuleHandle {}

impl ModuleHandle {
    unsafe fn from_library(
        library: Library,
        temp_dir: Option<tempfile::TempDir>,
    ) -> Result<Self> {
        let (destroy_fn, instance_ptr, metadata) = unsafe {
            let create_sym = library
                .get::<CreateModuleFn>(b"cspw_module_create\0")
                .context("Symbol 'cspw_module_create' not found in module")?;
            let create_fn = *create_sym;

            let destroy_sym = library
                .get::<DestroyModuleFn>(b"cspw_module_destroy\0")
                .context("Symbol 'cspw_module_destroy' not found in module")?;
            let destroy_fn = *destroy_sym;

            let instance_ptr = (create_fn)();
            if instance_ptr.is_null() {
                return Err(anyhow!("Module creation returned a null pointer"));
            }

            let module: &Box<dyn Module> = &*instance_ptr;
            let metadata = module.metadata();

            (destroy_fn, instance_ptr, metadata)
        };

        Ok(Self {
            _library: library,
            instance_ptr,
            destroy_fn,
            metadata,
            _temp_dir: temp_dir,
        })
    }

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

    /// Load a compiled dynamic library (.dll / .so / .dylib) from a file path
    pub fn load_module<P: AsRef<Path>>(&mut self, path: P) -> Result<&ModuleMetadata> {
        let path_ref = path.as_ref();
        let library = unsafe {
            Library::new(path_ref)
                .with_context(|| format!("Failed to load dynamic library at {:?}", path_ref))?
        };

        let handle = unsafe { ModuleHandle::from_library(library, None)? };
        let name = handle.metadata.name.clone();
        self.modules.insert(name.clone(), handle);
        Ok(&self.modules.get(&name).unwrap().metadata)
    }

    /// Load a compiled dynamic library directly from file bytes (.dll / .so / .dylib)
    pub fn load_from_bytes(&mut self, bytes: &[u8]) -> Result<&ModuleMetadata> {
        let temp_dir = tempfile::Builder::new()
            .prefix("cspw_module_")
            .tempdir()
            .context("Failed to create temporary directory for module")?;

        let ext = std::env::consts::DLL_EXTENSION;
        let file_path = temp_dir.path().join(format!("module.{}", ext));

        std::fs::write(&file_path, bytes)
            .with_context(|| format!("Failed to write module bytes to {:?}", file_path))?;

        let library = unsafe {
            Library::new(&file_path)
                .with_context(|| format!("Failed to load dynamic library from {:?}", file_path))?
        };

        let handle = unsafe { ModuleHandle::from_library(library, Some(temp_dir))? };
        let name = handle.metadata.name.clone();
        self.modules.insert(name.clone(), handle);
        Ok(&self.modules.get(&name).unwrap().metadata)
    }

    /// Inspect a compiled dynamic library directly from file bytes to extract its metadata without keeping it loaded
    pub fn inspect_from_bytes(&self, bytes: &[u8]) -> Result<ModuleMetadata> {
        let temp_dir = tempfile::Builder::new()
            .prefix("cspw_module_inspect_")
            .tempdir()
            .context("Failed to create temporary directory for module inspection")?;

        let ext = std::env::consts::DLL_EXTENSION;
        let file_path = temp_dir.path().join(format!("module_inspect.{}", ext));

        std::fs::write(&file_path, bytes)
            .with_context(|| format!("Failed to write module bytes to {:?}", file_path))?;

        let library = unsafe {
            Library::new(&file_path)
                .with_context(|| format!("Failed to load dynamic library from {:?}", file_path))?
        };

        let handle = unsafe { ModuleHandle::from_library(library, Some(temp_dir))? };
        Ok(handle.metadata.clone())
    }

    /// Unload a loaded module by its name
    pub fn unload_module(&mut self, name: &str) -> Result<()> {
        if self.modules.remove(name).is_some() {
            Ok(())
        } else {
            Err(anyhow!("Module '{}' not loaded", name))
        }
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