//! Engine for restbook.
//!
//! This crate is UI-agnostic and owns no runtime. Everything that the desktop
//! shell needs flows through [`RequestDef`] -> [`prepare`] -> [`Transport`] ->
//! [`ResponseRecord`], with history persisted through [`Store`].

pub mod error;
pub mod model;
pub mod prepare;
pub mod store;
pub mod template;
pub mod transport;

pub use error::{Error, Result};
pub use model::{
    ApiKeyLocation, Auth, Body, Environment, FormPart, Kv, Method, RequestDef, ResponseRecord,
};
pub use prepare::{prepare, PartData, PreparedBody, PreparedPart, PreparedRequest};
pub use store::{HistoryEntry, HistoryHit, Store};
pub use template::{resolve, VarContext};
pub use transport::{CancelToken, Transport};
