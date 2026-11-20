//! WASM engine configuration and AOT compilation.
//!
//! This module provides the core wasmtime engine with settings optimized
//! for the Barbacane plugin runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::{
    Config, Engine, Instance, InstanceAllocationStrategy, InstancePre, Linker, Module, OptLevel,
    PoolConcurrencyLimitError, PoolingAllocationConfig, Store,
};

use crate::error::WasmError;
use crate::instance::{add_host_functions, PluginState};
use crate::limits::PluginLimits;

/// Wall-clock granularity of the epoch ticker. The per-call epoch deadline is
/// expressed as a count of these ticks, so it bounds the slack between a
/// plugin's configured execution budget and when it is actually interrupted.
pub(crate) const EPOCH_TICK: Duration = Duration::from_millis(1);

/// Size of a WASM page, in bytes.
const WASM_PAGE: usize = 65536;

/// Bytes of a pooled instance's memory that are reset in place, page by
/// dirty page, when the instance ends; past them the pages are returned to
/// the kernel and fault back in on the slot's next use. Resetting in place
/// needs Linux 6.7 or later (`PAGEMAP_SCAN`); elsewhere every page is
/// returned.
const POOL_KEEP_RESIDENT: usize = 0;

/// The instance pool an engine keeps: memory, tables and instance slots set up
/// once and reused, instead of mapped and unmapped for each instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSize {
    /// Instances alive at once that the pool holds. Each reserves virtual
    /// address space for one linear memory (about 4 GiB on 64-bit hosts).
    pub instances: u32,
    /// The largest linear memory an instance may grow to, in bytes.
    pub max_memory_bytes: usize,
}

/// An engine and the host functions linked into it.
struct Host {
    engine: Engine,
    linker: Linker<PluginState>,
}

impl Host {
    fn new(config: &Config) -> Result<Self, WasmError> {
        let engine =
            Engine::new(config).map_err(|e| WasmError::EngineCreation(format!("{e:#}")))?;
        let mut linker = Linker::new(&engine);
        add_host_functions(&mut linker)?;
        Ok(Self { engine, linker })
    }

    /// `wasm` compiled for this engine and linked against the host functions.
    fn compile(&self, wasm: &[u8]) -> Result<Linked, wasmtime::Error> {
        let module = Module::new(&self.engine, wasm)?;
        let pre = self
            .linker
            .instantiate_pre(&module)
            .map_err(|e| e.to_string());
        Ok(Linked { module, pre })
    }
}

/// A module compiled for one engine and linked against its host functions.
#[derive(Clone)]
struct Linked {
    module: Module,
    /// The module linked against the host functions, once for all its
    /// instances, or why it cannot be.
    pre: Result<InstancePre<PluginState>, String>,
}

/// The per-request copy of a pooled module, for instances the full pool
/// cannot hold. Compiled the first time it is needed.
struct Overflow {
    wasm: Arc<[u8]>,
    host: Arc<Host>,
    linked: OnceLock<Result<Linked, String>>,
}

impl Overflow {
    fn linked(&self) -> Result<&InstancePre<PluginState>, WasmError> {
        let linked = self
            .linked
            .get_or_init(|| self.host.compile(&self.wasm).map_err(|e| format!("{e:#}")));
        match linked {
            Ok(linked) => linked
                .pre
                .as_ref()
                .map_err(|e| WasmError::Instantiation(e.clone())),
            Err(e) => Err(WasmError::Compilation(e.clone())),
        }
    }
}

/// A store and the instance in it, and whether the instance was allocated
/// per request because the pool was full.
pub(crate) struct Instantiated {
    pub(crate) store: Store<PluginState>,
    pub(crate) instance: Instance,
    pub(crate) pool_was_full: bool,
}

/// A compiled WASM module ready for instantiation.
#[derive(Clone)]
pub struct CompiledModule {
    /// Where instances come from: the pooled engine when the engine has a
    /// pool and the module fits it, the per-request engine otherwise.
    primary: Linked,
    /// Per-request allocation for instances a full pool cannot hold, when
    /// `primary` is pooled.
    overflow: Option<Arc<Overflow>>,
    /// Whether an instance can start from the memory `init` left (see
    /// [`crate::snapshot::replayable`]).
    replayable: bool,
    /// The plugin name this module belongs to.
    pub name: String,
    /// The plugin version.
    pub version: String,
    /// Whether this plugin needs the request body in `on_request`.
    pub body_access: bool,
}

impl CompiledModule {
    /// Get a reference to the underlying wasmtime module.
    pub fn module(&self) -> &Module {
        &self.primary.module
    }

    /// Whether this module's instances come from the engine's pool.
    pub fn pooled(&self) -> bool {
        self.overflow.is_some()
    }

    /// Whether an instance can start from the memory `init` left.
    pub(crate) fn replayable(&self) -> bool {
        self.replayable
    }

    /// A new instance running with `limits`: from the pool when there is one
    /// and it has room, allocated for this instance alone otherwise.
    pub(crate) fn instantiate(
        &self,
        state: PluginState,
        limits: &PluginLimits,
    ) -> Result<Instantiated, WasmError> {
        let pre = self
            .primary
            .pre
            .as_ref()
            .map_err(|e| WasmError::Instantiation(e.clone()))?;
        let (error, state) = match start(pre, state, limits) {
            Ok((store, instance)) => {
                return Ok(Instantiated {
                    store,
                    instance,
                    pool_was_full: false,
                })
            }
            Err(failed) => failed,
        };
        let pool_full = error
            .chain()
            .any(|cause| cause.is::<PoolConcurrencyLimitError>());
        match &self.overflow {
            Some(overflow) if pool_full => {
                let (store, instance) = start(overflow.linked()?, state, limits)
                    .map_err(|(e, _)| WasmError::Instantiation(format!("{e:#}")))?;
                Ok(Instantiated {
                    store,
                    instance,
                    pool_was_full: true,
                })
            }
            _ => Err(WasmError::Instantiation(format!("{error:#}"))),
        }
    }
}

/// Instantiate `pre` in a new store holding `state`, with `limits`' fuel and
/// deadline. On failure, the error and the state, for another attempt.
#[allow(clippy::result_large_err)]
fn start(
    pre: &InstancePre<PluginState>,
    state: PluginState,
    limits: &PluginLimits,
) -> Result<(Store<PluginState>, Instance), (wasmtime::Error, PluginState)> {
    let mut store = Store::new(pre.module().engine(), state);
    if let Err(e) = store.set_fuel(limits.max_fuel) {
        return Err((e.context("failed to set fuel"), store.into_data()));
    }
    // Wall-clock backstop: trap if the guest runs past its time budget. The
    // deadline is expressed in epoch ticks (see EPOCH_TICK).
    store.set_epoch_deadline(limits.max_execution_ms.max(1));
    store.limiter(|state| state);
    match pre.instantiate(&mut store) {
        Ok(instance) => Ok((store, instance)),
        Err(e) => Err((e, store.into_data())),
    }
}

/// The WASM engine that compiles and manages plugin modules.
pub struct WasmEngine {
    /// Allocates each instance's memory when the instance is created.
    on_demand: Arc<Host>,
    /// Allocates instances from a pool, when the engine has one.
    pooled: Option<Arc<Host>>,
    /// The instances the pool holds at once; 0 without a pool.
    pool_instances: u32,
    limits: PluginLimits,
    /// Signals the epoch ticker thread to stop on drop.
    epoch_stop: Arc<AtomicBool>,
    /// Handle to the background epoch ticker thread.
    epoch_ticker: Option<JoinHandle<()>>,
}

impl Drop for WasmEngine {
    fn drop(&mut self) {
        self.epoch_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.epoch_ticker.take() {
            let _ = handle.join();
        }
    }
}

impl WasmEngine {
    /// Create a new WASM engine with default configuration.
    pub fn new() -> Result<Self, WasmError> {
        Self::with_limits(PluginLimits::default())
    }

    /// Create a new WASM engine with custom resource limits.
    pub fn with_limits(limits: PluginLimits) -> Result<Self, WasmError> {
        Self::with_pool(limits, None)
    }

    /// Create a WASM engine whose instances come from a pool of `pool` size,
    /// with per-request allocation for the instances a full pool cannot hold.
    ///
    /// A pool the host cannot reserve address space for (an address-space
    /// limit, a 39-bit address space) is logged and left out: every instance
    /// is then allocated per request.
    pub fn with_pool(limits: PluginLimits, pool: Option<PoolSize>) -> Result<Self, WasmError> {
        let config = Self::config(&limits);
        let on_demand = Arc::new(Host::new(&config)?);
        let pooled = match pool.filter(|pool| pool.instances > 0) {
            None => None,
            Some(pool) => match Host::new(&Self::pooled_config(&config, pool)) {
                Ok(host) => Some((Arc::new(host), pool.instances)),
                Err(e) => {
                    tracing::warn!(
                        instances = pool.instances,
                        error = %e,
                        "cannot set up the plugin instance pool; every plugin instance is allocated per request"
                    );
                    None
                }
            },
        };
        let (pooled, pool_instances) = match pooled {
            Some((host, instances)) => (Some(host), instances),
            None => (None, 0),
        };

        // Spawn the epoch ticker: it increments the engines' epoch every
        // `EPOCH_TICK` so per-call epoch deadlines fire on a wall-clock basis.
        let epoch_stop = Arc::new(AtomicBool::new(false));
        let epoch_ticker = {
            let engines: Vec<Engine> = std::iter::once(&on_demand)
                .chain(pooled.as_ref())
                .map(|host| host.engine.clone())
                .collect();
            let stop = Arc::clone(&epoch_stop);
            std::thread::Builder::new()
                .name("wasm-epoch-ticker".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(EPOCH_TICK);
                        for engine in &engines {
                            engine.increment_epoch();
                        }
                    }
                })
                .map_err(|e| WasmError::EngineCreation(e.to_string()))?
        };

        Ok(Self {
            on_demand,
            pooled,
            pool_instances,
            limits,
            epoch_stop,
            epoch_ticker: Some(epoch_ticker),
        })
    }

    /// The engine configuration both engines share. Compiled code depends on
    /// it, not on the allocation strategy.
    fn config(limits: &PluginLimits) -> Config {
        let mut config = Config::new();

        // Use Cranelift for AOT compilation with speed optimization
        config.cranelift_opt_level(OptLevel::Speed);

        // Enable fuel consumption for execution time limiting
        config.consume_fuel(true);

        // Enable epoch interruption as a wall-clock backstop. Fuel bounds the
        // number of instructions executed, but not wall-clock time (e.g. on a
        // slow host, or if fuel is miscalibrated). A background thread ticks the
        // engine epoch and each call sets a deadline, trapping a guest that runs
        // past its time budget.
        config.epoch_interruption(true);

        // Configure memory settings
        config.max_wasm_stack(limits.max_stack_bytes);

        // Enable reference types (for modern WASM features)
        config.wasm_reference_types(true);

        // Enable bulk memory operations
        config.wasm_bulk_memory(true);

        // Enable multi-value returns
        config.wasm_multi_value(true);

        // Disable WASM features we don't need
        config.wasm_threads(false);

        config
    }

    /// `config` with instances allocated from a pool of `pool` size.
    fn pooled_config(config: &Config, pool: PoolSize) -> Config {
        let mut pooling = PoolingAllocationConfig::default();
        pooling
            .total_core_instances(pool.instances)
            .total_memories(pool.instances)
            .total_tables(pool.instances)
            .total_gc_heaps(pool.instances)
            .max_memory_size(pool.max_memory_bytes.div_ceil(WASM_PAGE) * WASM_PAGE)
            .linear_memory_keep_resident(POOL_KEEP_RESIDENT)
            .pagemap_scan(wasmtime::Enabled::Auto);
        let mut config = config.clone();
        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling));
        config
    }

    /// The instances the pool holds at once; 0 without a pool.
    pub fn pool_instances(&self) -> u32 {
        self.pool_instances
    }

    /// Get the configured resource limits.
    pub fn limits(&self) -> &PluginLimits {
        &self.limits
    }

    /// AOT-compile a WASM module from bytes.
    ///
    /// The compiled module can be instantiated multiple times efficiently.
    /// With a pool, the module is compiled for the pooled engine; one the pool
    /// cannot hold (a memory or table larger than its slots) is compiled for
    /// per-request allocation instead, and says so in a warning.
    pub fn compile(
        &self,
        wasm_bytes: &[u8],
        name: String,
        version: String,
        body_access: bool,
    ) -> Result<CompiledModule, WasmError> {
        let (primary, overflow) = match &self.pooled {
            Some(pooled) => match pooled.compile(wasm_bytes) {
                Ok(linked) => {
                    let overflow = Overflow {
                        wasm: Arc::from(wasm_bytes),
                        host: Arc::clone(&self.on_demand),
                        linked: OnceLock::new(),
                    };
                    (linked, Some(Arc::new(overflow)))
                }
                Err(pool_error) => {
                    let linked = self.compile_on_demand(wasm_bytes)?;
                    tracing::warn!(
                        plugin = %name,
                        reason = %format!("{pool_error:#}"),
                        "plugin does not fit the instance pool; its instances are allocated per request"
                    );
                    (linked, None)
                }
            },
            None => (self.compile_on_demand(wasm_bytes)?, None),
        };

        Ok(CompiledModule {
            primary,
            overflow,
            replayable: crate::snapshot::replayable(wasm_bytes),
            name,
            version,
            body_access,
        })
    }

    fn compile_on_demand(&self, wasm_bytes: &[u8]) -> Result<Linked, WasmError> {
        self.on_demand
            .compile(wasm_bytes)
            .map_err(|e| WasmError::Compilation(e.to_string()))
    }

    /// Validate a WASM module without fully compiling it.
    ///
    /// This is faster than full compilation and useful for quick validation.
    pub fn validate(&self, wasm_bytes: &[u8]) -> Result<(), WasmError> {
        Module::validate(&self.on_demand.engine, wasm_bytes)
            .map_err(|e| WasmError::Compilation(e.to_string()))
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new().expect("failed to create default WASM engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid WASM module (empty module)
    const MINIMAL_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // magic number
        0x01, 0x00, 0x00, 0x00, // version
    ];

    #[test]
    fn create_engine() {
        let engine = WasmEngine::new();
        assert!(engine.is_ok());
    }

    #[test]
    fn create_engine_with_limits() {
        let limits = PluginLimits::default().with_memory(32 * 1024 * 1024);
        let engine = WasmEngine::with_limits(limits);
        assert!(engine.is_ok());
    }

    #[test]
    fn validate_minimal_wasm() {
        let engine = WasmEngine::new().unwrap();
        assert!(engine.validate(MINIMAL_WASM).is_ok());
    }

    #[test]
    fn validate_invalid_wasm() {
        let engine = WasmEngine::new().unwrap();
        let invalid = &[0x00, 0x00, 0x00, 0x00];
        assert!(engine.validate(invalid).is_err());
    }

    #[test]
    fn compile_minimal_wasm() {
        let engine = WasmEngine::new().unwrap();
        let result = engine.compile(MINIMAL_WASM, "test".into(), "1.0.0".into(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn compiled_module_has_metadata() {
        let engine = WasmEngine::new().unwrap();
        let module = engine
            .compile(MINIMAL_WASM, "my-plugin".into(), "2.1.0".into(), false)
            .unwrap();
        assert_eq!(module.name, "my-plugin");
        assert_eq!(module.version, "2.1.0");
    }
}
