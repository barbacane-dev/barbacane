//! Barbacane data plane library.
//!
//! The entry point is the `barbacane` binary. These modules are public so the
//! integration tests can reach them; they are not a stable API
//! ([stability policy](https://docs.barbacane.dev/reference/stability.html)).

#[doc(hidden)]
pub mod admin;
#[doc(hidden)]
pub mod control;
#[doc(hidden)]
pub mod control_plane;
#[doc(hidden)]
pub mod dev;
#[doc(hidden)]
pub mod hot_reload;
#[doc(hidden)]
pub mod router;
#[doc(hidden)]
pub mod validator;
