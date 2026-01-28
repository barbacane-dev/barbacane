<p align="center">
  <img src="assets/img/barbacane-icon_black.png" alt="Barbacane" width="200">
</p>

<h1 align="center">Barbacane</h1>

<p align="center"><i>Your spec is your gateway.</i></p>

---

Barbacane is a spec-driven API gateway built in Rust. Point it at an OpenAPI or AsyncAPI spec and it becomes your gateway — routing, validation, authentication, and all. No proprietary config language, no drift between your spec and your infrastructure.

- **Spec as config** — Your OpenAPI 3.x or AsyncAPI 3.x specification is the single source of truth. No separate gateway DSL to maintain.
- **Fast and predictable** — Built on Rust, Tokio, and Hyper. No garbage collector, no latency surprises.
- **Secure by default** — Memory-safe runtime, TLS via Rustls, sandboxed WASM plugins, secrets never baked into artifacts.
- **Edge-ready** — Stateless data plane instances designed to run close to your users, with a separate control plane handling compilation and distribution.
- **Extensible** — Write plugins in any language that compiles to WebAssembly. They run in a sandbox, so a buggy plugin can't take down the gateway.
