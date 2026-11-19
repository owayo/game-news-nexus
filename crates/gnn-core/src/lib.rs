//! Game News Nexus のドメインモデル・取得定義・カタログ・SQLite ストア。
//!
//! 通信と UI には依存しない。取得処理は `gnn-fetch`、画面は `gnn-app` が持つ。

pub mod agenda;
pub mod catalog;
pub mod config;
pub mod entry;
pub mod image_ref;
pub mod legacy;
pub mod net;
pub mod schedule;
pub mod source;
pub mod source_v1;
pub mod store;
pub mod subscription;
pub mod templates;
pub mod text;
pub mod yaml;

pub use catalog::{Catalog, CatalogEntry, ResolvedSource, SourceOrigin};
pub use config::{AppPaths, AppSettings, ConfigStore, Secrets, ThemePreference};
pub use entry::{Entry, EntryDraft, EntrySummary, Published, TimePrecision};
pub use image_ref::{ImageIdentity, ImageObservation};
pub use source::{Channel, ListKind, SourceConfig};
pub use store::{
    AnalysisInput, AnalysisResult, AnalysisStatus, ApplyReport, EntryQuery, EntrySchedule,
    FetchRun, ImageHolder, KnownItem, RunStatus, SavedAnalyses, SourceState, Store,
};
pub use subscription::{Subscription, Subscriptions};
