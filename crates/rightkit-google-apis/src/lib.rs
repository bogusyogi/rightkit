//! `rightkit-google-apis`: async Google Workspace REST clients on one shared, retrying client.
//!
//! - [`google`]: the shared client (`Google`), auth header resolution, retry and backoff, deadlines.
//! - [`oauth`]: desktop PKCE sign-in and refresh-token access-token resolution (`AccessTokens`),
//!   with the Workspace scope set ([`oauth::SCOPE`]).
//! - [`gmail`]: message list/fetch/batch fetch, full and history sync, MIME build and parse,
//!   compose/send; plus [`labels`], [`filters`], [`mail_settings`] (vacation, signatures),
//!   [`drafts`] and [`attachments`].
//! - [`calendar`], [`drive`], [`people`] (contacts, groups, recipients), [`tasks`] and [`meet`].
//!
//! Tokens never leave the process except to Google; the caller stores refresh tokens.

pub mod attachments;
pub mod calendar;
pub mod drafts;
pub mod drive;
pub mod filters;
pub mod gmail;
pub mod google;
pub mod labels;
pub mod mail_settings;
pub mod meet;
pub mod oauth;
pub mod people;
pub mod tasks;
