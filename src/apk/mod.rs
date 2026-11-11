#![allow(unused_imports)]

pub mod axml;
pub mod compat;
pub mod desktop;
pub mod inspector;

pub use axml::{AxmlAttribute, AxmlElement, AxmlParser};
pub use compat::{
    classify_pm_install_failure, container_supports_arm, evaluate_abi_status,
    is_package_archive_path, merge_prop_package_list, normalize_abi, resolve_window_dimensions,
    select_install_splits, stage_bundle_artifacts, AbiStatus, AdaptationProfile, ApkBundleAnalyzer,
    ApkBundleEntry, ApkCompatibilityReport, BundleFormat, BundleInspection, ObbEntry,
    OrientationHint, PmInstallDiagnosis, ProfileOverrides, ProfileStore, SplitKind, StagingPlan,
    WindowMode,
};
pub use desktop::DesktopIntegration;
pub use inspector::{ApkInspector, ApkMetadata};
