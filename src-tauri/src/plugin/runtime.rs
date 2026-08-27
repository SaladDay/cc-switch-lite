use std::{collections::HashMap, fs::File, io::Read, path::Path, sync::Mutex};

use wasmtime::{
    component::{Component, Linker, Val},
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
};

use super::{package::MAX_PACKAGE_BYTES, PluginError, PluginRequest, PluginResponse};

const MAX_COMPONENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEMORY_BYTES: usize = 16 * 1024 * 1024;
const MAX_TABLE_ELEMENTS: usize = 10_000;
const MAX_INSTANCES: usize = 16;
const MAX_MEMORIES: usize = 4;
const MAX_TABLES: usize = 8;
const FUEL_PER_CALL: u64 = 10_000_000;
const FUEL_PER_CURRENT_CALL: u64 = 1_000_000;
const MAX_CACHED_COMPONENTS: usize = 32;

struct RuntimeState {
    limits: StoreLimits,
}

pub struct PluginRuntime {
    engine: Engine,
    components: Mutex<HashMap<String, Component>>,
}

impl PluginRuntime {
    pub fn new() -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_component_model(true);
        config.wasm_multi_memory(false);
        let engine = Engine::new(&config).map_err(|_| PluginError::Runtime)?;
        Ok(Self {
            engine,
            components: Mutex::new(HashMap::new()),
        })
    }

    pub fn validate_component(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> Result<(), PluginError> {
        let component = self.component(path, expected_sha256)?;
        let mut store = self.store(FUEL_PER_CALL)?;
        let instance = Linker::new(&self.engine)
            .instantiate(&mut store, &component)
            .map_err(|_| PluginError::Runtime)?;
        let function = instance
            .get_func(&mut store, "invoke")
            .ok_or(PluginError::Runtime)?;
        let params = function.params(&store);
        let results = function.results(&store);
        let result_shape_matches = matches!(
            results.first(),
            Some(wasmtime::component::types::Type::Result(result))
                if matches!(result.ok(), Some(wasmtime::component::types::Type::String))
                    && matches!(result.err(), Some(wasmtime::component::types::Type::String))
        );
        if params.len() != 1
            || !matches!(params[0].1, wasmtime::component::types::Type::String)
            || results.len() != 1
            || !result_shape_matches
        {
            return Err(PluginError::Runtime);
        }
        Ok(())
    }

    pub fn invoke(
        &self,
        component_path: &Path,
        expected_sha256: &str,
        request: &PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        self.invoke_with_fuel(component_path, expected_sha256, request, FUEL_PER_CALL)
    }

    pub fn invoke_current(
        &self,
        component_path: &Path,
        expected_sha256: &str,
        request: &PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        self.invoke_with_fuel(
            component_path,
            expected_sha256,
            request,
            FUEL_PER_CURRENT_CALL,
        )
    }

    fn invoke_with_fuel(
        &self,
        component_path: &Path,
        expected_sha256: &str,
        request: &PluginRequest,
        fuel: u64,
    ) -> Result<PluginResponse, PluginError> {
        let request = serde_json::to_string(request).map_err(PluginError::Serialize)?;
        if request.len() > MAX_ENVELOPE_BYTES {
            return Err(PluginError::Invalid(
                "plugin request exceeds the envelope limit".to_owned(),
            ));
        }

        let component = self.component(component_path, expected_sha256)?;
        let mut store = self.store(fuel)?;
        let instance = Linker::new(&self.engine)
            .instantiate(&mut store, &component)
            .map_err(|_| PluginError::Runtime)?;
        let function = instance
            .get_func(&mut store, "invoke")
            .ok_or(PluginError::Runtime)?;
        let params = [Val::String(request)];
        let mut results = [Val::Result(Ok(Some(Box::new(Val::String(String::new())))))];
        function
            .call(&mut store, &params, &mut results)
            .map_err(|_| PluginError::Runtime)?;
        function
            .post_return(&mut store)
            .map_err(|_| PluginError::Runtime)?;

        let response = match results.into_iter().next() {
            Some(Val::Result(Ok(Some(value)))) => match *value {
                Val::String(response) => response,
                _ => return Err(PluginError::Runtime),
            },
            Some(Val::Result(Err(_))) => return Err(PluginError::Runtime),
            _ => return Err(PluginError::Runtime),
        };
        if response.len() > MAX_ENVELOPE_BYTES {
            return Err(PluginError::Runtime);
        }
        serde_json::from_str(&response).map_err(|_| PluginError::Runtime)
    }

    fn component(&self, path: &Path, expected_sha256: &str) -> Result<Component, PluginError> {
        if let Some(component) = self
            .components
            .lock()
            .map_err(|_| PluginError::Runtime)?
            .get(expected_sha256)
            .cloned()
        {
            return Ok(component);
        }
        let metadata =
            std::fs::symlink_metadata(path).map_err(|source| super::io_error(path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(PluginError::InvalidState(
                "installed component is not a regular file".to_owned(),
            ));
        }
        if metadata.len() > MAX_COMPONENT_BYTES as u64 || metadata.len() > MAX_PACKAGE_BYTES as u64
        {
            return Err(PluginError::InvalidState(
                "installed component exceeds the size limit".to_owned(),
            ));
        }
        let file = File::open(path).map_err(|source| super::io_error(path, source))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((MAX_COMPONENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| super::io_error(path, source))?;
        if bytes.len() > MAX_COMPONENT_BYTES {
            return Err(PluginError::InvalidState(
                "installed component exceeds the size limit".to_owned(),
            ));
        }
        if crate::operation::sha256(&bytes) != expected_sha256 {
            return Err(PluginError::InvalidState(
                "installed component digest does not match its manifest".to_owned(),
            ));
        }
        let component = Component::new(&self.engine, &bytes).map_err(|_| PluginError::Runtime)?;
        let mut components = self.components.lock().map_err(|_| PluginError::Runtime)?;
        if components.len() >= MAX_CACHED_COMPONENTS {
            components.clear();
        }
        components.insert(expected_sha256.to_owned(), component.clone());
        Ok(component)
    }

    fn store(&self, fuel: u64) -> Result<Store<RuntimeState>, PluginError> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .table_elements(MAX_TABLE_ELEMENTS)
            .instances(MAX_INSTANCES)
            .memories(MAX_MEMORIES)
            .tables(MAX_TABLES)
            .build();
        let mut store = Store::new(&self.engine, RuntimeState { limits });
        store.limiter(|state| &mut state.limits);
        store.set_fuel(fuel).map_err(|_| PluginError::Runtime)?;
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("plugin-fixture.wasm")
    }

    #[test]
    fn invokes_a_component_built_from_the_public_wit_contract() {
        let path = fixture();
        let bytes = std::fs::read(&path).expect("read component fixture");
        let digest = crate::operation::sha256(&bytes);
        let runtime = PluginRuntime::new().expect("create plugin runtime");

        runtime
            .validate_component(&path, &digest)
            .expect("validate component interface");
        let response = runtime
            .invoke(
                &path,
                &digest,
                &PluginRequest::Validate {
                    contract_major: 1,
                    app_id: "claude".to_owned(),
                    adapter_id: "example.claude".to_owned(),
                    settings: Map::new(),
                },
            )
            .expect("invoke fixture");

        assert_eq!(response, PluginResponse::Valid);
        assert_eq!(runtime.components.lock().unwrap().len(), 1);
    }

    #[test]
    fn rejects_a_component_whose_digest_changed() {
        let runtime = PluginRuntime::new().expect("create plugin runtime");

        let result = runtime.validate_component(&fixture(), &"0".repeat(64));

        assert!(matches!(result, Err(PluginError::InvalidState(_))));
    }
}
