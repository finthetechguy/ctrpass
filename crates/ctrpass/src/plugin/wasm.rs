//! WebAssembly plugins, each call run in a fresh sandbox with no imports.

use wasmi::{Config, Linker, Memory, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

use crate::Error;
use crate::title::Values;

/// Instructions one call may run.
#[cfg(not(test))]
const FUEL: u64 = 500_000_000;
#[cfg(test)]
const FUEL: u64 = 5_000_000;
/// Memory one call may use.
const MEMORY: usize = 64 << 20;
/// Largest output one call may return.
const MAX_OUTPUT: usize = 16 << 20;

const EXPORTS: [&str; 6] = [
    "ctrpass_alloc",
    "ctrpass_manifest",
    "ctrpass_create",
    "ctrpass_read",
    "ctrpass_write",
    "ctrpass_problems",
];

pub(crate) struct Module {
    name: String,
    engine: wasmi::Engine,
    module: wasmi::Module,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module({})", self.name)
    }
}

struct Sandbox {
    limits: StoreLimits,
}

impl Module {
    pub(crate) fn new(wasm: &[u8]) -> Result<Self, Error> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = wasmi::Engine::new(&config);
        let module = wasmi::Module::new(&engine, wasm)
            .map_err(|e| Error::Format(format!("not a usable module: {e}")))?;
        if let Some(i) = module.imports().next() {
            return Err(Error::Format(format!(
                "plugins may not import anything, but this one imports {}.{}",
                i.module(),
                i.name()
            )));
        }
        let exports: Vec<&str> = module.exports().map(|e| e.name()).collect();
        if let Some(missing) = EXPORTS
            .iter()
            .chain(&["memory"])
            .find(|e| !exports.contains(e))
        {
            return Err(Error::Format(format!(
                "the module does not export {missing}"
            )));
        }
        Ok(Module {
            name: "plugin".into(),
            engine,
            module,
        })
    }

    pub(crate) fn named(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    fn fail(&self, e: impl std::fmt::Display) -> Error {
        Error::Format(format!("{} plugin failed: {e}", self.name))
    }

    /// Runs `func` on `inputs` in a new instance, returning its result bytes.
    fn call(&self, func: &str, inputs: &[&[u8]]) -> Result<Vec<u8>, Error> {
        let mut store = Store::new(
            &self.engine,
            Sandbox {
                limits: StoreLimitsBuilder::new()
                    .memory_size(MEMORY)
                    .instances(1)
                    .build(),
            },
        );
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL).map_err(|e| self.fail(e))?;
        let instance = Linker::<Sandbox>::new(&self.engine)
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| self.fail(e))?;
        let memory: Memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| self.fail("no memory"))?;
        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func(&store, "ctrpass_alloc")
            .map_err(|e| self.fail(e))?;
        let mut args = Vec::new();
        for input in inputs {
            let len = i32::try_from(input.len()).map_err(|_| self.fail("input too large"))?;
            let ptr = alloc.call(&mut store, len).map_err(|e| self.fail(e))?;
            memory
                .write(&mut store, ptr as u32 as usize, input)
                .map_err(|e| self.fail(e))?;
            args.extend([ptr, len]);
        }
        let out: i64 = match args[..] {
            [] => instance
                .get_typed_func::<(), i64>(&store, func)
                .and_then(|f| f.call(&mut store, ())),
            [a, b] => instance
                .get_typed_func::<(i32, i32), i64>(&store, func)
                .and_then(|f| f.call(&mut store, (a, b))),
            [a, b, c, d] => instance
                .get_typed_func::<(i32, i32, i32, i32), i64>(&store, func)
                .and_then(|f| f.call(&mut store, (a, b, c, d))),
            _ => unreachable!("calls take at most two inputs"),
        }
        .map_err(|e| self.fail(e))?;
        let (ptr, len) = (
            (out as u64 >> 32) as usize,
            (out as u64 & 0xffff_ffff) as usize,
        );
        if len == 0 || len > MAX_OUTPUT {
            return Err(self.fail(format!("{func} returned {len} bytes")));
        }
        let mut buf = vec![0; len];
        memory
            .read(&store, ptr, &mut buf)
            .map_err(|e| self.fail(e))?;
        match buf[0] {
            0 => Ok(buf.split_off(1)),
            1 => Err(Error::Format(format!(
                "{}: {}",
                self.name,
                String::from_utf8_lossy(&buf[1..])
            ))),
            n => Err(self.fail(format!("{func} returned status {n}"))),
        }
    }

    fn json<T: serde::de::DeserializeOwned>(&self, func: &str, out: &[u8]) -> Result<T, Error> {
        serde_json::from_slice(out).map_err(|e| self.fail(format!("{func} returned {e}")))
    }

    pub(crate) fn manifest(&self) -> Result<Vec<u8>, Error> {
        self.call("ctrpass_manifest", &[])
    }

    pub(crate) fn create(&self, identity: &Values) -> Result<Vec<u8>, Error> {
        let input = serde_json::to_vec(identity).expect("values serialise");
        self.call("ctrpass_create", &[&input])
    }

    pub(crate) fn read(&self, body: &[u8]) -> Result<Values, Error> {
        let out = self.call("ctrpass_read", &[body])?;
        self.json("ctrpass_read", &out)
    }

    pub(crate) fn write(&self, body: &[u8], values: &Values) -> Result<Vec<u8>, Error> {
        let input = serde_json::to_vec(values).expect("values serialise");
        self.call("ctrpass_write", &[body, &input])
    }

    pub(crate) fn problems(&self, body: &[u8]) -> Result<Vec<String>, Error> {
        let out = self.call("ctrpass_problems", &[body])?;
        self.json("ctrpass_problems", &out)
    }
}
