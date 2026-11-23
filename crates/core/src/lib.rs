//! Engine for restbook.
//!
//! This crate is UI-agnostic and owns no runtime. Everything that the desktop
//! shell needs flows through [`RequestDef`] -> [`prepare`] -> [`Transport`] ->
//! [`ResponseRecord`], with history persisted through [`Store`].

pub mod error;
pub mod generator;
pub mod import;
pub mod kv_text;
pub mod model;
pub mod net;
pub mod oauth;
pub mod prepare;
pub mod store;
pub mod template;
pub mod testing;
pub mod transport;
pub mod vars;

pub use error::{Error, Result};
pub use generator::Generators;
pub use model::{
    ApiKeyLocation, Auth, Body, Collection, CollectionItem, Environment, FileBody, Folder,
    FormPart, Kv, Method, RedirectHop, RequestDef, ResponseBody, ResponseRecord, SavedRequest,
};
pub use net::{ClientCert, NetworkConfig, Proxy};
pub use oauth::{
    parse_token_response, AuthorizationRequest, OAuthConfig, OAuthGrant, OAuthTransport, Pkce,
    TokenRequest, TokenResponse,
};
pub use prepare::{
    default_headers, prepare, PartData, PreparedBody, PreparedPart, PreparedRequest,
};
pub use store::{Draft, HistoryEntry, HistoryHit, Origin, Store};
pub use template::{resolve, VarContext};
pub use testing::FakeTransport;
pub use transport::{CancelToken, Progress, ProgressSnapshot, Transport};
pub use vars::{Preview, ResolvedVar, ScopedVars, VarPreview, VarScope};
