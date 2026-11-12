#![allow(dead_code)]

use super::axml::{AxmlElement, AxmlParser};
use super::inspector::{ApkInspector, ApkMetadata};
use crate::runtime::ConfigManager;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Supported Android application package / bundle archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleFormat {
    SingleApk,
    Xapk,
    Apks,
    Apkm,
    ApkBundleZip,
}

impl BundleFormat {
    pub fn display_label(self) -> &'static str {
        match self {
            Self::SingleApk => "APK",
            Self::Xapk => "XAPK (APKPure bundle)",
            Self::Apks => "APKS (Split APK bundle)",
            Self::Apkm => "APKM (APKMirror bundle)",
            Self::ApkBundleZip => "ZIP App Bundle",
        }
    }
}

/// Logical role of an APK entry inside a split bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SplitKind {
    Base,
    Abi(String),
    Density(String),
    Language(String),
    Feature(String),
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkBundleEntry {
    pub entry_name: String,
    pub kind: SplitKind,
    pub size_bytes: u64,
    pub split_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObbEntry {
    pub entry_name: String,
    pub package_name: String,
    pub file_name: String,
    pub install_rel_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AbiStatus {
    PureDex,
    NativeX86 {
        abis: Vec<String>,
    },
    ArmTranslationRequired {
        abis: Vec<String>,
        container_supports_arm: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrientationHint {
    Unspecified,
    Portrait,
    Landscape,
    Sensor,
}

impl OrientationHint {
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
            Self::Sensor => "sensor",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkCompatibilityReport {
    pub format: BundleFormat,
    pub native_abis: Vec<String>,
    pub abi_status: AbiStatus,
    pub min_sdk: Option<u32>,
    pub target_sdk: Option<u32>,
    pub compile_sdk: Option<u32>,
    pub orientation: OrientationHint,
    pub resizeable_activity: Option<bool>,
    pub is_game: bool,
    pub is_test_only: bool,
    pub requires_touchscreen: bool,
    pub game_engine: Option<String>,
    pub uses_gms: bool,
    pub gms_signals: Vec<String>,
    pub uses_network_state: bool,
    pub obb_files: Vec<ObbEntry>,
    pub splits: Vec<ApkBundleEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BundleInspection {
    pub source_path: PathBuf,
    pub metadata: ApkMetadata,
    pub compat: ApkCompatibilityReport,
    pub recommended_profile: AdaptationProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    Auto,
    Freeform,
    SingleWindow,
    PhonePortrait,
    TabletLandscape,
    CustomSize,
}

impl Default for WindowMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptationProfile {
    pub package_name: String,
    #[serde(default)]
    pub window_mode: WindowMode,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub lcd_density: Option<u32>,
    #[serde(default)]
    pub fake_wifi: bool,
    #[serde(default)]
    pub fake_touch: bool,
    #[serde(default = "default_true")]
    pub auto_grant_permissions: bool,
    #[serde(default = "default_play_store")]
    pub installer_spoof: Option<String>,
    #[serde(default)]
    pub abi_override: Option<String>,
    #[serde(default = "default_true")]
    pub allow_test_only: bool,
    #[serde(default)]
    pub allow_downgrade: bool,
    #[serde(default)]
    pub force_sdk: bool,
    #[serde(default)]
    pub virtual_keyboard: Option<bool>,
    #[serde(default)]
    pub extra_props: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

fn default_play_store() -> Option<String> {
    Some("com.android.vending".to_string())
}

impl Default for AdaptationProfile {
    fn default() -> Self {
        Self {
            package_name: String::new(),
            window_mode: WindowMode::Auto,
            width: None,
            height: None,
            lcd_density: None,
            fake_wifi: false,
            fake_touch: false,
            auto_grant_permissions: true,
            installer_spoof: default_play_store(),
            abi_override: None,
            allow_test_only: true,
            allow_downgrade: false,
            force_sdk: false,
            virtual_keyboard: None,
            extra_props: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProfileOverrides {
    pub window_mode: Option<WindowMode>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lcd_density: Option<u32>,
    pub fake_wifi: Option<bool>,
    pub fake_touch: Option<bool>,
    pub auto_grant_permissions: Option<bool>,
    pub installer_spoof: Option<Option<String>>,
    pub abi_override: Option<Option<String>>,
    pub allow_downgrade: Option<bool>,
    pub force_sdk: Option<bool>,
    pub virtual_keyboard: Option<Option<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmInstallDiagnosis {
    pub error_code: Option<String>,
    pub summary_ru: String,
    pub retry_with_downgrade: bool,
    pub retry_with_force_sdk: bool,
    pub retry_with_bypass_low_sdk: bool,
    pub retry_without_grant: bool,
}

#[derive(Debug, Clone)]
pub struct StagingPlan {
    pub staging_dir: PathBuf,
    pub apk_paths: Vec<PathBuf>,
    pub obb_files: Vec<(PathBuf, String)>,
}

#[derive(Debug, Deserialize)]
struct XapkManifestJson {
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version_code: Option<serde_json::Value>,
    #[serde(default)]
    version_name: Option<String>,
    #[serde(default)]
    split_apks: Vec<XapkSplitApkJson>,
    #[serde(default)]
    expansions: Vec<XapkExpansionJson>,
}

#[derive(Debug, Deserialize)]
struct XapkSplitApkJson {
    #[serde(default)]
    file: String,
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
struct XapkExpansionJson {
    #[serde(default)]
    file: String,
    #[serde(default)]
    install_path: String,
}

#[derive(Debug, Deserialize)]
struct ApkmInfoJson {
    #[serde(default)]
    pname: Option<String>,
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    release_version: Option<String>,
    #[serde(default)]
    versioncode: Option<serde_json::Value>,
}

const MAX_IN_MEMORY_BASE_APK_BYTES: usize = 256 * 1024 * 1024;

const ENGINE_SIGNATURES: &[(&str, &str)] = &[
    ("libunity.so", "Unity"),
    ("libue4.so", "Unreal Engine"),
    ("libunreal.so", "Unreal Engine"),
    ("libgodot_android.so", "Godot"),
    ("libcocos2d", "Cocos2d"),
    ("libgdx.so", "libGDX"),
    ("libflutter.so", "Flutter"),
    ("libreactnativejni.so", "React Native"),
];

const ABI_ALIASES: &[(&str, &str)] = &[
    ("arm64_v8a", "arm64-v8a"),
    ("arm64-v8a", "arm64-v8a"),
    ("arm64", "arm64-v8a"),
    ("armeabi_v7a", "armeabi-v7a"),
    ("armeabi-v7a", "armeabi-v7a"),
    ("armeabi", "armeabi"),
    ("x86_64", "x86_64"),
    ("x86", "x86"),
];

const DENSITY_TOKENS: &[&str] = &[
    "ldpi", "mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi", "tvdpi", "nodpi", "anydpi",
];

const GMS_METADATA_MARKERS: &[&str] = &[
    "com.google.android.gms.version",
    "com.google.firebase.",
    "com.google.android.geo.api_key",
    "com.google.android.maps.v2.api_key",
];

const GMS_PERMISSION_MARKERS: &[&str] = &[
    "com.google.android.c2dm.permission.receive",
    "com.android.vending.billing",
    "com.google.android.finsky.permission.bind_get_install_referrer_service",
];

const PROP_VALUE_MAX_LEN: usize = 91;

/// Returns true if the target string clearly points to an Android archive or bundle path.
pub fn is_package_archive_path(target: &str) -> bool {
    let lower = target.to_lowercase();
    lower.ends_with(".apk")
        || lower.ends_with(".xapk")
        || lower.ends_with(".apks")
        || lower.ends_with(".apkm")
        || lower.ends_with(".zip")
        || Path::new(target).is_file()
}

/// Normalizes ABI tokens (converting bundletool underscores like `arm64_v8a` -> `arm64-v8a`).
pub fn normalize_abi(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    for (token, canonical) in ABI_ALIASES {
        if lower == *token {
            return (*canonical).to_string();
        }
    }
    lower
}

/// Classifies split tags from AXML `split` attributes or filenames (`split_config.arm64_v8a.apk`).
pub fn classify_split_tag(tag_or_name: &str) -> SplitKind {
    let file_name = Path::new(tag_or_name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(tag_or_name);
    let lower_name = file_name.to_ascii_lowercase();
    let normalized = lower_name.strip_suffix(".apk").unwrap_or(&lower_name);

    if normalized == "base"
        || normalized == "base-master"
        || normalized == "master"
        || normalized.ends_with(".master")
    {
        return SplitKind::Base;
    }

    let (candidate, had_split_prefix) = if let Some(s) = normalized
        .strip_prefix("split_config.")
        .or_else(|| normalized.strip_prefix("config."))
        .or_else(|| normalized.strip_prefix("split_"))
        .or_else(|| normalized.strip_prefix("base-"))
    {
        (s, true)
    } else {
        (normalized, false)
    };

    for (token, canonical) in ABI_ALIASES {
        if candidate == *token || candidate.ends_with(&format!(".{}", token)) {
            return SplitKind::Abi((*canonical).to_string());
        }
    }

    for dpi in DENSITY_TOKENS {
        if candidate == *dpi || candidate.ends_with(&format!(".{}", dpi)) {
            return SplitKind::Density((*dpi).to_string());
        }
    }

    // Two-letter ISO 639-1 language code (e.g. "en", "ru", "de") or region variant ("pt_br").
    // Ignore trailing country TLDs on full package filenames (e.g. "com.example.ru.apk").
    let lang_part = candidate.rsplit('.').next().unwrap_or(candidate);
    if is_language_code(lang_part) && (had_split_prefix || candidate == lang_part) {
        return SplitKind::Language(lang_part.to_string());
    }

    if !candidate.is_empty() {
        return SplitKind::Feature(candidate.to_string());
    }

    SplitKind::Other
}

fn is_language_code(s: &str) -> bool {
    if s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase()) {
        return true;
    }
    if let Some((lang, region)) = s.split_once('_').or_else(|| s.split_once('-')) {
        return (2..=3).contains(&lang.len())
            && lang.bytes().all(|b| b.is_ascii_lowercase())
            && (2..=4).contains(&region.len())
            && region.bytes().all(|b| b.is_ascii_alphanumeric());
    }
    false
}

/// Checks if any container ABI belongs to ARM / ARM64.
pub fn container_supports_arm(container_abis: &[String]) -> bool {
    container_abis.iter().any(|abi| {
        let a = normalize_abi(abi);
        a == "arm64-v8a" || a == "armeabi-v7a" || a == "armeabi"
    })
}

pub struct ApkBundleAnalyzer;

impl ApkBundleAnalyzer {
    /// Inspects a single `.apk` or a split bundle (`.xapk`, `.apks`, `.apkm`, `.zip`) against container ABIs.
    pub fn inspect<P: AsRef<Path>>(path: P, container_abis: &[String]) -> Result<BundleInspection> {
        let path_ref = path.as_ref();
        let file = File::open(path_ref)
            .with_context(|| format!("Failed to open APK file: {}", path_ref.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("Failed to read APK as ZIP archive: {}", path_ref.display()))?;

        if archive.by_name("AndroidManifest.xml").is_ok() {
            let metadata = ApkInspector::inspect(path_ref)?;
            let compat = analyze_apk_archive(
                &mut archive,
                BundleFormat::SingleApk,
                Vec::new(),
                Vec::new(),
                &metadata,
                container_abis,
            )?;
            let recommended_profile = AdaptationProfile::recommend(&metadata.package_name, &compat);
            return Ok(BundleInspection {
                source_path: path_ref.to_path_buf(),
                metadata,
                compat,
                recommended_profile,
            });
        }

        inspect_bundle_archive(path_ref, &mut archive, container_abis)
    }
}

fn detect_bundle_format(path: &Path, has_xapk_json: bool, has_apkm_json: bool) -> BundleFormat {
    if has_xapk_json {
        return BundleFormat::Xapk;
    }
    if has_apkm_json {
        return BundleFormat::Apkm;
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("xapk") => BundleFormat::Xapk,
        Some("apks") => BundleFormat::Apks,
        Some("apkm") => BundleFormat::Apkm,
        _ => BundleFormat::ApkBundleZip,
    }
}

fn read_zip_entry_bytes<R: Read + Seek>(archive: &mut ZipArchive<R>, name: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("Запись '{}' не найдена в архиве", name))?;
    if entry.size() as usize > max_bytes {
        bail!("Файл '{}' слишком велик для чтения в память ({} байт)", name, entry.size());
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

fn inspect_bundle_archive(
    source_path: &Path,
    archive: &mut ZipArchive<File>,
    container_abis: &[String],
) -> Result<BundleInspection> {
    let mut xapk_manifest: Option<XapkManifestJson> = None;
    let mut apkm_info: Option<ApkmInfoJson> = None;
    let mut apk_entries_raw: Vec<(String, u64)> = Vec::new();
    let mut obb_entries: Vec<ObbEntry> = Vec::new();

    for idx in 0..archive.len() {
        let entry = archive.by_index(idx)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        let size = entry.size();
        drop(entry);

        if lower == "manifest.json" {
            if let Ok(bytes) = read_zip_entry_bytes(archive, &name, 8 * 1024 * 1024) {
                xapk_manifest = serde_json::from_slice(&bytes).ok();
            }
        } else if lower == "info.json" {
            if let Ok(bytes) = read_zip_entry_bytes(archive, &name, 8 * 1024 * 1024) {
                apkm_info = serde_json::from_slice(&bytes).ok();
            }
        } else if lower.ends_with(".apk") {
            apk_entries_raw.push((name, size));
        } else if lower.ends_with(".obb") {
            let file_name = Path::new(&name)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("main.obb")
                .to_string();
            let pkg_hint = infer_pkg_from_obb_path(&name);
            obb_entries.push(ObbEntry {
                entry_name: name.clone(),
                package_name: pkg_hint,
                file_name,
                install_rel_path: name,
                size_bytes: size,
            });
        }
    }

    if apk_entries_raw.is_empty() {
        bail!(
            "В архиве '{}' не найден AndroidManifest.xml и не обнаружены внутренние .apk файлы",
            source_path.display()
        );
    }

    let format = detect_bundle_format(source_path, xapk_manifest.is_some(), apkm_info.is_some());
    let xapk_split_ids: BTreeMap<String, String> = xapk_manifest
        .as_ref()
        .map(|m| {
            m.split_apks
                .iter()
                .map(|s| (s.file.clone(), s.id.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut base_candidate: Option<(String, Vec<u8>)> = None;
    let mut splits: Vec<ApkBundleEntry> = Vec::new();
    let mut split_abis_collected: BTreeSet<String> = BTreeSet::new();
    let mut split_engines: Vec<String> = Vec::new();

    // Order base-looking candidates first so we find the primary APK quickly.
    apk_entries_raw.sort_by_key(|(name, _)| {
        let fname = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name)
            .to_ascii_lowercase();
        let priority = if fname == "base.apk" || fname == "base-master.apk" {
            0
        } else if xapk_split_ids.get(name).map(|id| id == "base").unwrap_or(false) {
            1
        } else if !fname.contains("config.") && !fname.starts_with("split_") {
            2
        } else {
            3
        };
        (priority, name.clone())
    });

    for (entry_name, size_bytes) in apk_entries_raw {
        let apk_bytes = read_zip_entry_bytes(archive, &entry_name, MAX_IN_MEMORY_BASE_APK_BYTES)?;
        let mut inner_zip = ZipArchive::new(Cursor::new(&apk_bytes))
            .with_context(|| format!("Некорректный внутренний APK '{}'", entry_name))?;

        let (split_attr, abis, engine) = peek_inner_apk(&mut inner_zip)?;
        for a in abis {
            split_abis_collected.insert(a);
        }
        if let Some(eng) = engine {
            if !split_engines.contains(&eng) {
                split_engines.push(eng);
            }
        }

        let inferred_kind = if let Some(ref sp) = split_attr {
            classify_split_tag(sp)
        } else if let Some(id) = xapk_split_ids.get(&entry_name) {
            classify_split_tag(id)
        } else {
            classify_split_tag(&entry_name)
        };

        let file_lower = Path::new(&entry_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_named_split = file_lower.starts_with("config.")
            || file_lower.starts_with("split_config.")
            || file_lower.starts_with("split_");

        let is_explicit_base = matches!(inferred_kind, SplitKind::Base)
            || xapk_split_ids
                .get(&entry_name)
                .map(|id| id == "base")
                .unwrap_or(false)
            || (base_candidate.is_none()
                && split_attr.is_none()
                && !is_named_split
                && !matches!(
                    inferred_kind,
                    SplitKind::Abi(_) | SplitKind::Density(_) | SplitKind::Language(_)
                ));

        let kind = if is_explicit_base {
            SplitKind::Base
        } else {
            inferred_kind
        };

        if let SplitKind::Abi(ref abi) = kind {
            split_abis_collected.insert(abi.clone());
        }

        if is_explicit_base && base_candidate.is_none() {
            base_candidate = Some((entry_name.clone(), apk_bytes));
        }

        splits.push(ApkBundleEntry {
            entry_name,
            kind,
            size_bytes,
            split_name: split_attr,
        });
    }

    let (base_name, base_bytes) = match base_candidate {
        Some(found) => found,
        None => {
            let fallback_name = splits[0].entry_name.clone();
            splits[0].kind = SplitKind::Base;
            let bytes = read_zip_entry_bytes(archive, &fallback_name, MAX_IN_MEMORY_BASE_APK_BYTES)?;
            (fallback_name, bytes)
        }
    };

    let mut base_zip = ZipArchive::new(Cursor::new(&base_bytes))?;
    let mut metadata = inspect_zip_metadata(source_path, &mut base_zip)?;

    // Enrich metadata from bundle JSON manifests when AXML resources use references (@0x...).
    if let Some(ref xm) = xapk_manifest {
        if let Some(ref pkg) = xm.package_name {
            if metadata.package_name == "unknown.package" || metadata.package_name.is_empty() {
                metadata.package_name = pkg.clone();
            }
        }
        if let Some(ref name) = xm.name {
            if is_fallback_label(&metadata.app_label, &metadata.package_name) {
                metadata.app_label = Some(name.clone());
            }
        }
        if metadata.version_name.is_none() {
            metadata.version_name = xm.version_name.clone();
        }
        if metadata.version_code.is_none() {
            metadata.version_code = xm.version_code.as_ref().map(json_value_to_string);
        }
        for exp in &xm.expansions {
            if let Some(existing) = obb_entries.iter_mut().find(|o| o.entry_name == exp.file) {
                if !exp.install_path.is_empty() {
                    existing.install_rel_path = exp.install_path.clone();
                }
                existing.package_name = metadata.package_name.clone();
            }
        }
    }
    if let Some(ref am) = apkm_info {
        if let Some(ref pkg) = am.pname {
            if metadata.package_name == "unknown.package" || metadata.package_name.is_empty() {
                metadata.package_name = pkg.clone();
            }
        }
        if let Some(ref name) = am.app_name {
            if is_fallback_label(&metadata.app_label, &metadata.package_name) {
                metadata.app_label = Some(name.clone());
            }
        }
        if metadata.version_name.is_none() {
            metadata.version_name = am.release_version.clone();
        }
        if metadata.version_code.is_none() {
            metadata.version_code = am.versioncode.as_ref().map(json_value_to_string);
        }
    }

    for obb in &mut obb_entries {
        if obb.package_name.is_empty() {
            obb.package_name = metadata.package_name.clone();
        }
    }

    let mut compat = analyze_apk_archive(
        &mut base_zip,
        format,
        splits,
        obb_entries,
        &metadata,
        container_abis,
    )?;

    // Merge ABI & engine knowledge gathered across ABI split APKs.
    let mut merged_abis: BTreeSet<String> = compat.native_abis.iter().cloned().collect();
    merged_abis.extend(split_abis_collected);
    compat.native_abis = merged_abis.into_iter().collect();
    compat.abi_status = evaluate_abi_status(&compat.native_abis, container_abis);
    if compat.game_engine.is_none() {
        compat.game_engine = split_engines.into_iter().next();
    }
    compat.warnings = build_compat_warnings(&compat, Some(&base_name));

    let recommended_profile = AdaptationProfile::recommend(&metadata.package_name, &compat);
    Ok(BundleInspection {
        source_path: source_path.to_path_buf(),
        metadata,
        compat,
        recommended_profile,
    })
}

fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn is_fallback_label(label: &Option<String>, package_name: &str) -> bool {
    match label {
        None => true,
        Some(l) => l.is_empty() || package_name.split('.').last() == Some(l.as_str()),
    }
}

fn infer_pkg_from_obb_path(entry_path: &str) -> String {
    let parts: Vec<&str> = entry_path.split('/').collect();
    if let Some(pos) = parts.iter().position(|p| p.eq_ignore_ascii_case("obb")) {
        if let Some(pkg) = parts.get(pos + 1) {
            if pkg.contains('.') && !pkg.ends_with(".obb") {
                return (*pkg).to_string();
            }
        }
    }
    String::new()
}

fn peek_inner_apk<R: Read + Seek>(
    inner: &mut ZipArchive<R>,
) -> Result<(Option<String>, Vec<String>, Option<String>)> {
    let manifest_bytes = read_zip_entry_bytes(inner, "AndroidManifest.xml", 16 * 1024 * 1024)
        .context("Внутренний .apk не содержит AndroidManifest.xml")?;
    let mut parser = AxmlParser::new(&manifest_bytes);
    let root = parser.parse()?;
    let split_attr = root.get_attr("split", parser.string_pool());
    let (abis, engine) = scan_native_libs_and_engine(inner);
    Ok((split_attr, abis, engine))
}

fn inspect_zip_metadata<R: Read + Seek>(source_path: &Path, archive: &mut ZipArchive<R>) -> Result<ApkMetadata> {
    let manifest_bytes = read_zip_entry_bytes(archive, "AndroidManifest.xml", 16 * 1024 * 1024)?;
    let mut parser = AxmlParser::new(&manifest_bytes);
    let root = parser.parse()?;
    let pool = parser.string_pool();

    let package_name = root
        .get_attr("package", pool)
        .unwrap_or_else(|| "unknown.package".to_string());
    let version_code = root.get_attr("versionCode", pool);
    let version_name = root.get_attr("versionName", pool);

    let permissions = root
        .find_children("uses-permission")
        .into_iter()
        .filter_map(|c| c.get_attr("name", pool))
        .collect::<Vec<_>>();

    let mut app_label = None;
    let mut main_activity = None;
    for app in root.find_children("application") {
        if let Some(lbl) = app.get_attr("label", pool) {
            if !lbl.starts_with('@') && !lbl.is_empty() {
                app_label = Some(lbl);
            }
        }
        for act in app
            .find_children("activity")
            .into_iter()
            .chain(app.find_children("activity-alias"))
        {
            if is_launcher_activity(act, pool) && main_activity.is_none() {
                if let Some(act_name) = act
                    .get_attr("name", pool)
                    .or_else(|| act.get_attr("targetActivity", pool))
                {
                    let norm = ApkInspector::normalize_activity_name(&package_name, &act_name);
                    if !norm.is_empty() {
                        main_activity = Some(norm);
                    }
                }
            }
        }
    }

    let (icon_path_in_apk, icon_data) = extract_best_icon(archive);
    let label = app_label.unwrap_or_else(|| {
        package_name
            .split('.')
            .last()
            .unwrap_or("Android App")
            .to_string()
    });

    Ok(ApkMetadata {
        file_path: source_path.to_string_lossy().to_string(),
        package_name,
        version_code,
        version_name,
        app_label: Some(label),
        main_activity,
        permissions,
        icon_path_in_apk,
        icon_data,
    })
}

fn is_launcher_activity(act: &AxmlElement, pool: &[String]) -> bool {
    for filter in act.find_children("intent-filter") {
        let is_main = filter
            .find_children("action")
            .into_iter()
            .any(|a| a.get_attr("name", pool).as_deref() == Some("android.intent.action.MAIN"));
        let is_launcher = filter
            .find_children("category")
            .into_iter()
            .any(|c| c.get_attr("name", pool).as_deref() == Some("android.intent.category.LAUNCHER"));
        if is_main && is_launcher {
            return true;
        }
    }
    false
}

fn extract_best_icon<R: Read + Seek>(archive: &mut ZipArchive<R>) -> (Option<String>, Option<Vec<u8>>) {
    let icon_candidates = [
        "res/mipmap-xxxhdpi-v4/ic_launcher.png",
        "res/mipmap-xxxhdpi/ic_launcher.png",
        "res/mipmap-xxhdpi-v4/ic_launcher.png",
        "res/mipmap-xxhdpi/ic_launcher.png",
        "res/mipmap-xhdpi-v4/ic_launcher.png",
        "res/mipmap-xhdpi/ic_launcher.png",
        "res/mipmap-hdpi-v4/ic_launcher.png",
        "res/mipmap-hdpi/ic_launcher.png",
        "res/mipmap-mdpi-v4/ic_launcher.png",
        "res/mipmap-mdpi/ic_launcher.png",
        "res/drawable-xxxhdpi-v4/ic_launcher.png",
        "res/drawable-xxhdpi-v4/ic_launcher.png",
        "res/drawable-xhdpi-v4/ic_launcher.png",
        "res/drawable/ic_launcher.png",
    ];
    for candidate in icon_candidates {
        if let Ok(mut f) = archive.by_name(candidate) {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                return (Some(candidate.to_string()), Some(buf));
            }
        }
    }
    for i in 0..archive.len() {
        if let Ok(mut f) = archive.by_index(i) {
            let name = f.name().to_string();
            let lower = name.to_ascii_lowercase();
            if (lower.contains("ic_launcher") || lower.contains("icon"))
                && (lower.ends_with(".png") || lower.ends_with(".webp"))
            {
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                    return (Some(name), Some(buf));
                }
            }
        }
    }
    (None, None)
}

fn analyze_apk_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    format: BundleFormat,
    splits: Vec<ApkBundleEntry>,
    obb_files: Vec<ObbEntry>,
    metadata: &ApkMetadata,
    container_abis: &[String],
) -> Result<ApkCompatibilityReport> {
    let manifest_bytes = read_zip_entry_bytes(archive, "AndroidManifest.xml", 16 * 1024 * 1024)?;
    let mut parser = AxmlParser::new(&manifest_bytes);
    let root = parser.parse()?;
    let pool = parser.string_pool();

    let (native_abis, game_engine) = scan_native_libs_and_engine(archive);
    let abi_status = evaluate_abi_status(&native_abis, container_abis);

    let compile_sdk = parse_u32_attr(&root, "compileSdkVersion", pool);
    let mut min_sdk = None;
    let mut target_sdk = None;
    if let Some(uses_sdk) = root.find_children("uses-sdk").into_iter().next() {
        min_sdk = parse_u32_attr(uses_sdk, "minSdkVersion", pool);
        target_sdk = parse_u32_attr(uses_sdk, "targetSdkVersion", pool);
    }

    let mut requires_touchscreen = false;
    let mut has_gamepad_feature = false;
    let mut uses_gles_game = false;
    for feat in root.find_children("uses-feature") {
        let req = feat
            .get_attr("required", pool)
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        if let Some(name) = feat.get_attr("name", pool) {
            if name == "android.hardware.touchscreen" && req {
                requires_touchscreen = true;
            }
            if name == "android.hardware.gamepad" {
                has_gamepad_feature = true;
            }
        }
        if let Some(gl) = feat.get_attr("glEsVersion", pool) {
            if let Ok(ver) = u32::from_str_radix(gl.trim_start_matches("0x"), 16) {
                if ver >= 0x0003_0000 {
                    uses_gles_game = true;
                }
            }
        }
    }

    let mut orientation = OrientationHint::Unspecified;
    let mut resizeable_activity = None;
    let is_game_engine = matches!(
        game_engine.as_deref(),
        Some("Unity" | "Unreal Engine" | "Godot" | "Cocos2d")
    );
    let mut is_game = has_gamepad_feature || is_game_engine || uses_gles_game;
    let mut is_test_only = false;
    let mut gms_signals = BTreeSet::new();

    if let Some(app) = root.find_children("application").into_iter().next() {
        if let Some(v) = parse_bool_attr(app, "resizeableActivity", pool) {
            resizeable_activity = Some(v);
        }
        if parse_bool_attr(app, "isGame", pool) == Some(true) {
            is_game = true;
        }
        if let Some(cat) = app.get_attr("appCategory", pool) {
            if cat == "0" || cat.eq_ignore_ascii_case("game") {
                is_game = true;
            }
        }
        if parse_bool_attr(app, "testOnly", pool) == Some(true) {
            is_test_only = true;
        }

        for lib in app.find_children("uses-library") {
            if let Some(name) = lib.get_attr("name", pool) {
                if name.contains("com.google.android.maps") || name.contains("com.google.android.gms") {
                    gms_signals.insert(format!("uses-library: {}", name));
                }
            }
        }
        for md in app.find_children("meta-data") {
            if let Some(name) = md.get_attr("name", pool) {
                let lower = name.to_ascii_lowercase();
                if GMS_METADATA_MARKERS.iter().any(|m| lower.starts_with(m)) {
                    gms_signals.insert(format!("meta-data: {}", name));
                }
            }
        }

        for act in app
            .find_children("activity")
            .into_iter()
            .chain(app.find_children("activity-alias"))
        {
            if !is_launcher_activity(act, pool) {
                continue;
            }
            if let Some(raw_orient) = act.get_attr("screenOrientation", pool) {
                orientation = parse_orientation_hint(&raw_orient);
            }
            if let Some(v) = parse_bool_attr(act, "resizeableActivity", pool) {
                resizeable_activity = Some(v);
            }
            break;
        }
    }

    for perm in &metadata.permissions {
        let lower = perm.to_ascii_lowercase();
        if GMS_PERMISSION_MARKERS.iter().any(|m| lower == *m) {
            gms_signals.insert(format!("permission: {}", perm));
        }
    }

    let uses_network_state = metadata.permissions.iter().any(|p| {
        p == "android.permission.ACCESS_WIFI_STATE"
            || p == "android.permission.ACCESS_NETWORK_STATE"
            || p == "android.permission.INTERNET"
    });

    let gms_vec: Vec<String> = gms_signals.into_iter().collect();
    let uses_gms = !gms_vec.is_empty();

    let mut report = ApkCompatibilityReport {
        format,
        native_abis,
        abi_status,
        min_sdk,
        target_sdk,
        compile_sdk,
        orientation,
        resizeable_activity,
        is_game,
        is_test_only,
        requires_touchscreen,
        game_engine,
        uses_gms,
        gms_signals: gms_vec,
        uses_network_state,
        obb_files,
        splits,
        warnings: Vec::new(),
    };
    report.warnings = build_compat_warnings(&report, None);
    Ok(report)
}

fn scan_native_libs_and_engine<R: Read + Seek>(archive: &mut ZipArchive<R>) -> (Vec<String>, Option<String>) {
    let mut abis = BTreeSet::new();
    let mut engine: Option<String> = None;

    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name();
            if let Some(rest) = name.strip_prefix("lib/") {
                if let Some((abi_raw, lib_file)) = rest.split_once('/') {
                    let abi = normalize_abi(abi_raw);
                    if !abi.is_empty() && name.ends_with(".so") {
                        abis.insert(abi);
                    }
                    if engine.is_none() {
                        let lower_lib = lib_file.to_ascii_lowercase();
                        for (sig, engine_name) in ENGINE_SIGNATURES {
                            if lower_lib.contains(sig) {
                                engine = Some((*engine_name).to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    (abis.into_iter().collect(), engine)
}

pub fn evaluate_abi_status(native_abis: &[String], container_abis: &[String]) -> AbiStatus {
    if native_abis.is_empty() {
        return AbiStatus::PureDex;
    }
    let normalized_native: Vec<String> = native_abis.iter().map(|a| normalize_abi(a)).collect();
    let has_x86 = normalized_native.iter().any(|a| a == "x86_64" || a == "x86");
    if has_x86 {
        return AbiStatus::NativeX86 {
            abis: normalized_native,
        };
    }
    AbiStatus::ArmTranslationRequired {
        abis: normalized_native,
        container_supports_arm: container_supports_arm(container_abis),
    }
}

fn build_compat_warnings(report: &ApkCompatibilityReport, _base_name: Option<&str>) -> Vec<String> {
    let mut warnings = Vec::new();
    if let AbiStatus::ArmTranslationRequired {
        container_supports_arm: false,
        ref abis,
    } = report.abi_status
    {
        warnings.push(format!(
            "ARM-only пакет ({}), а в Waydroid не активна ARM-трансляция — выполните `rustydroid setup-arm`.",
            abis.join(", ")
        ));
    }
    if let Some(t) = report.target_sdk {
        if t < 23 {
            warnings.push(format!(
                "Низкий targetSdkVersion ({}), на Android 13+ может потребоваться `--force-sdk`.",
                t
            ));
        }
    }
    if report.uses_gms {
        warnings.push(
            "Обнаружена зависимость от Google Play Services / Firebase (на VANILLA-образе Waydroid без microG/GApps часть функций может не работать).".to_string()
        );
    }
    if !report.obb_files.is_empty() {
        warnings.push(format!(
            "В комплекте {} OBB-файл(ов) кэша — RustyDroid автоматически развернёт их в /sdcard/Android/obb.",
            report.obb_files.len()
        ));
    }
    if report.is_test_only {
        warnings.push("Пакет помечен android:testOnly=true — установка будет выполнена с флагом `-t`.".to_string());
    }
    warnings
}

fn parse_u32_attr(elem: &AxmlElement, attr: &str, pool: &[String]) -> Option<u32> {
    elem.get_attr(attr, pool)?.trim().parse::<u32>().ok()
}

fn parse_bool_attr(elem: &AxmlElement, attr: &str, pool: &[String]) -> Option<bool> {
    match elem.get_attr(attr, pool)?.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "-1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_orientation_hint(raw: &str) -> OrientationHint {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "1" | "7" | "9" | "12" | "portrait" | "sensorportrait" | "reverseportrait" | "userportrait" => {
            OrientationHint::Portrait
        }
        "0" | "6" | "8" | "11" | "landscape" | "sensorlandscape" | "reverselandscape" | "userlandscape" => {
            OrientationHint::Landscape
        }
        "2" | "4" | "10" | "13" | "sensor" | "fullsensor" | "fulluser" | "user" => OrientationHint::Sensor,
        _ => OrientationHint::Unspecified,
    }
}

impl AdaptationProfile {
    /// Produces smart adaptation defaults tailored to an APK's compatibility report.
    pub fn recommend(package_name: &str, compat: &ApkCompatibilityReport) -> Self {
        let window_mode = match compat.orientation {
            OrientationHint::Portrait if compat.resizeable_activity == Some(false) => {
                WindowMode::PhonePortrait
            }
            OrientationHint::Landscape if compat.is_game => WindowMode::TabletLandscape,
            _ => WindowMode::Auto,
        };

        let force_sdk = compat.target_sdk.map(|s| s < 23).unwrap_or(false);
        let fake_touch = compat.is_game || (compat.requires_touchscreen && compat.orientation == OrientationHint::Landscape);
        let fake_wifi = compat.is_game && compat.uses_network_state;

        Self {
            package_name: package_name.to_string(),
            window_mode,
            width: None,
            height: None,
            lcd_density: None,
            fake_wifi,
            fake_touch,
            auto_grant_permissions: true,
            installer_spoof: default_play_store(),
            abi_override: None,
            allow_test_only: true,
            allow_downgrade: false,
            force_sdk,
            virtual_keyboard: None,
            extra_props: BTreeMap::new(),
        }
    }

    /// Applies optional per-run overrides onto the profile.
    pub fn apply_overrides(&mut self, overrides: &ProfileOverrides) {
        if let Some(mode) = overrides.window_mode {
            self.window_mode = mode;
        }
        if let Some(w) = overrides.width {
            self.width = Some(w);
        }
        if let Some(h) = overrides.height {
            self.height = Some(h);
        }
        if overrides.width.is_some() && overrides.height.is_some() && overrides.window_mode.is_none() {
            self.window_mode = WindowMode::CustomSize;
        }
        if let Some(dpi) = overrides.lcd_density {
            self.lcd_density = Some(dpi);
        }
        if let Some(fw) = overrides.fake_wifi {
            self.fake_wifi = fw;
        }
        if let Some(ft) = overrides.fake_touch {
            self.fake_touch = ft;
        }
        if let Some(ag) = overrides.auto_grant_permissions {
            self.auto_grant_permissions = ag;
        }
        if let Some(ref spoof) = overrides.installer_spoof {
            self.installer_spoof = spoof.clone();
        }
        if let Some(ref abi) = overrides.abi_override {
            self.abi_override = abi.clone().map(|s| normalize_abi(&s));
        }
        if let Some(ad) = overrides.allow_downgrade {
            self.allow_downgrade = ad;
        }
        if let Some(fs) = overrides.force_sdk {
            self.force_sdk = fs;
        }
        if let Some(vk) = overrides.virtual_keyboard {
            self.virtual_keyboard = vk;
        }
    }
}

/// Manages per-application adaptation profiles in `<config_dir>/profiles/<pkg>.json`.
pub struct ProfileStore;

impl ProfileStore {
    pub fn profiles_dir() -> PathBuf {
        ConfigManager::get_config_dir().join("profiles")
    }

    pub fn profile_path(package_name: &str) -> PathBuf {
        let sanitized: String = package_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self::profiles_dir().join(format!("{}.json", sanitized))
    }

    pub fn load(package_name: &str) -> Option<AdaptationProfile> {
        let path = Self::profile_path(package_name);
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn save(profile: &AdaptationProfile) -> Result<PathBuf> {
        let dir = Self::profiles_dir();
        fs::create_dir_all(&dir)?;
        let path = Self::profile_path(&profile.package_name);
        let json = serde_json::to_string_pretty(profile)?;
        fs::write(&path, json)?;
        Ok(path)
    }

    pub fn remove(package_name: &str) -> Result<bool> {
        let path = Self::profile_path(package_name);
        if path.exists() {
            fs::remove_file(path)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn list_all() -> Vec<AdaptationProfile> {
        let dir = Self::profiles_dir();
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut profiles = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(text) = fs::read_to_string(p) {
                    if let Ok(prof) = serde_json::from_str::<AdaptationProfile>(&text) {
                        profiles.push(prof);
                    }
                }
            }
        }
        profiles.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        profiles
    }

    pub fn resolve_effective(
        package_name: &str,
        compat: Option<&ApkCompatibilityReport>,
        overrides: &ProfileOverrides,
    ) -> AdaptationProfile {
        let mut profile = Self::load(package_name).unwrap_or_else(|| match compat {
            Some(rep) => AdaptationProfile::recommend(package_name, rep),
            None => AdaptationProfile {
                package_name: package_name.to_string(),
                ..Default::default()
            },
        });
        profile.package_name = package_name.to_string();
        profile.apply_overrides(overrides);
        profile
    }
}

/// Selects the optimal subset of split APKs for the target container ABI, density, and locale.
pub fn select_install_splits(
    inspection: &BundleInspection,
    container_abis: &[String],
    abi_override: Option<&str>,
    preferred_lang: Option<&str>,
) -> Vec<String> {
    if inspection.compat.format == BundleFormat::SingleApk || inspection.compat.splits.is_empty() {
        return Vec::new();
    }

    let normalized_container: Vec<String> = container_abis.iter().map(|a| normalize_abi(a)).collect();
    let available_abis: BTreeSet<String> = inspection
        .compat
        .splits
        .iter()
        .filter_map(|s| match &s.kind {
            SplitKind::Abi(abi) => Some(abi.clone()),
            _ => None,
        })
        .collect();

    let chosen_abi: Option<String> = if let Some(forced) = abi_override.map(normalize_abi) {
        if available_abis.contains(&forced) {
            Some(forced)
        } else {
            None
        }
    } else {
        normalized_container
            .iter()
            .find(|c| available_abis.contains(*c))
            .cloned()
    };

    let available_densities: BTreeSet<String> = inspection
        .compat
        .splits
        .iter()
        .filter_map(|s| match &s.kind {
            SplitKind::Density(d) => Some(d.clone()),
            _ => None,
        })
        .collect();
    let preferred_densities = ["xhdpi", "xxhdpi", "hdpi", "mdpi", "nodpi", "xxxhdpi", "tvdpi"];
    let chosen_density: Option<String> = preferred_densities
        .iter()
        .find(|d| available_densities.contains(**d))
        .map(|s| (*s).to_string())
        .or_else(|| available_densities.iter().next().cloned());

    let host_lang = preferred_lang
        .map(|s| s.to_ascii_lowercase())
        .or_else(detect_host_language_code);
    let available_langs: BTreeSet<String> = inspection
        .compat
        .splits
        .iter()
        .filter_map(|s| match &s.kind {
            SplitKind::Language(l) => Some(l.clone()),
            _ => None,
        })
        .collect();

    let mut chosen_langs = BTreeSet::new();
    if let Some(ref l) = host_lang {
        if available_langs.contains(l) {
            chosen_langs.insert(l.clone());
        }
    }
    if available_langs.contains("en") {
        chosen_langs.insert("en".to_string());
    }
    if chosen_langs.is_empty() {
        if let Some(first_lang) = available_langs.iter().next() {
            chosen_langs.insert(first_lang.clone());
        }
    }

    let mut selected = Vec::new();
    for split in &inspection.compat.splits {
        let include = match &split.kind {
            SplitKind::Base | SplitKind::Feature(_) | SplitKind::Other => true,
            SplitKind::Abi(abi) => chosen_abi.as_deref() == Some(abi.as_str()),
            SplitKind::Density(d) => chosen_density.as_deref() == Some(d.as_str()),
            SplitKind::Language(lang) => chosen_langs.contains(lang),
        };
        if include {
            selected.push(split.entry_name.clone());
        }
    }

    // Ensure Base APK always comes first in the argument list for pm/adb install-multiple.
    selected.sort_by_key(|name| {
        let is_base = inspection
            .compat
            .splits
            .iter()
            .find(|s| &s.entry_name == name)
            .map(|s| matches!(s.kind, SplitKind::Base))
            .unwrap_or(false);
        (!is_base, name.clone())
    });

    selected
}

fn detect_host_language_code() -> Option<String> {
    let lang = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .ok()?;
    let short = lang.split(&['_', '.', '@', '-'][..]).next()?.to_ascii_lowercase();
    if short.len() == 2 && short.bytes().all(|b| b.is_ascii_lowercase()) {
        Some(short)
    } else {
        None
    }
}

/// Extracts selected APKs and OBB expansion files from a split bundle into `staging_dir`.
pub fn stage_bundle_artifacts(
    inspection: &BundleInspection,
    selected_entries: &[String],
    staging_dir: &Path,
) -> Result<StagingPlan> {
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(staging_dir);
    }
    fs::create_dir_all(staging_dir)?;

    if inspection.compat.format == BundleFormat::SingleApk {
        return Ok(StagingPlan {
            staging_dir: staging_dir.to_path_buf(),
            apk_paths: vec![inspection.source_path.clone()],
            obb_files: Vec::new(),
        });
    }

    let file = File::open(&inspection.source_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut apk_paths = Vec::new();

    for (idx, entry_name) in selected_entries.iter().enumerate() {
        let mut entry = archive
            .by_name(entry_name)
            .with_context(|| format!("Отсутствует split APK '{}' в архиве", entry_name))?;
        let base_fname = Path::new(entry_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("split.apk");
        let sanitized_fname: String = base_fname
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let target = staging_dir.join(format!("{:02}_{}", idx, sanitized_fname));
        let mut out = File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        apk_paths.push(target);
    }

    let mut obb_files = Vec::new();
    for obb in &inspection.compat.obb_files {
        let mut entry = archive
            .by_name(&obb.entry_name)
            .with_context(|| format!("Отсутствует OBB '{}' в архиве", obb.entry_name))?;
        let obb_stage_dir = staging_dir.join("obb").join(&obb.package_name);
        fs::create_dir_all(&obb_stage_dir)?;
        let host_obb = obb_stage_dir.join(&obb.file_name);
        let mut out = File::create(&host_obb)?;
        std::io::copy(&mut entry, &mut out)?;
        let remote_path = format!("/sdcard/Android/obb/{}/{}", obb.package_name, obb.file_name);
        obb_files.push((host_obb, remote_path));
    }

    Ok(StagingPlan {
        staging_dir: staging_dir.to_path_buf(),
        apk_paths,
        obb_files,
    })
}

/// Computes target `(width, height)` clamped cleanly within host bounds (`host_w x host_h`).
pub fn resolve_window_dimensions(
    profile: &AdaptationProfile,
    host_w: u32,
    host_h: u32,
) -> Option<(u32, u32)> {
    let safe_w = host_w.max(320);
    let safe_h = host_h.max(320);

    match profile.window_mode {
        WindowMode::Auto | WindowMode::Freeform | WindowMode::SingleWindow => {
            if let (Some(w), Some(h)) = (profile.width, profile.height) {
                Some(clamp_preserving_aspect(w, h, safe_w, safe_h))
            } else {
                None
            }
        }
        WindowMode::CustomSize => {
            let w = profile.width.unwrap_or(safe_w);
            let h = profile.height.unwrap_or(safe_h);
            Some(clamp_preserving_aspect(w, h, safe_w, safe_h))
        }
        WindowMode::PhonePortrait => {
            let target_h = profile.height.unwrap_or_else(|| safe_h.min(960));
            let target_w = profile.width.unwrap_or_else(|| ((target_h as f64) * 9.0 / 16.0).round() as u32);
            Some(clamp_preserving_aspect(target_w, target_h, safe_w, safe_h))
        }
        WindowMode::TabletLandscape => {
            let target_w = profile.width.unwrap_or_else(|| safe_w.min(1280));
            let target_h = profile.height.unwrap_or_else(|| ((target_w as f64) * 9.0 / 16.0).round() as u32);
            Some(clamp_preserving_aspect(target_w, target_h, safe_w, safe_h))
        }
    }
}

fn clamp_preserving_aspect(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let w = w.max(240);
    let h = h.max(240);
    let scale_w = (max_w as f64) / (w as f64);
    let scale_h = (max_h as f64) / (h as f64);
    let scale = scale_w.min(scale_h).min(1.0);
    (
        ((w as f64) * scale).round().max(240.0) as u32,
        ((h as f64) * scale).round().max(240.0) as u32,
    )
}

/// Safely updates a Waydroid comma-separated package list property within Android's 91-byte limit.
pub fn merge_prop_package_list(existing: &str, pkg: &str, enable: bool, max_len: usize) -> String {
    let max_len = if max_len == 0 { PROP_VALUE_MAX_LEN } else { max_len };
    let clean_existing: Vec<String> = existing
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    if clean_existing.iter().any(|s| s == "*") && enable {
        return "*".to_string();
    }

    let mut ordered = Vec::new();
    if enable {
        ordered.push(pkg.to_string());
    }
    for item in clean_existing {
        if item != pkg && item != "*" && !ordered.contains(&item) {
            ordered.push(item);
        }
    }

    let mut out = String::new();
    for item in ordered {
        let next_len = if out.is_empty() {
            item.len()
        } else {
            out.len() + 1 + item.len()
        };
        if next_len <= max_len {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(&item);
        }
    }
    out
}

/// Parses `pm install` / `adb install` output and classifies recoverable vs hard failures.
pub fn classify_pm_install_failure(output: &str) -> PmInstallDiagnosis {
    let upper = output.to_ascii_uppercase();
    let error_code = output
        .split('[')
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("INSTALL_"));

    if upper.contains("INSTALL_FAILED_VERSION_DOWNGRADE") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Установлена более новая версия пакета (downgrade).".to_string(),
            retry_with_downgrade: true,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_DEPRECATED_SDK_VERSION")
        || upper.contains("INSTALL_FAILED_TARGET_SDK_VERSION_MISMATCH")
    {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Устаревший targetSdkVersion блокируется Android без флага совместимости.".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: true,
            retry_with_bypass_low_sdk: true,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_NO_MATCHING_ABIS") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Нет совместимых нативных библиотек (ABI) в контейнере. Проверьте `rustydroid setup-arm` или флаг `--abi`.".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") || upper.contains("INSTALL_FAILED_SHARED_USER_INCOMPATIBLE") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Конфликт цифровой подписи с уже установленным приложением (удалите старую версию через `adb uninstall`).".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_TEST_ONLY") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Тестовая сборка (android:testOnly=true) требует флаг `-t`.".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_MISSING_SHARED_LIBRARY") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Пакет требует отсутствующую системную библиотеку (например, Google Maps / Play library).".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: false,
        };
    }

    if upper.contains("INSTALL_FAILED_PERMISSION_MODEL_DOWNGRADE") || upper.contains("SECURITYEXCEPTION") && upper.contains("GRANT") {
        return PmInstallDiagnosis {
            error_code,
            summary_ru: "Ограничение выдачи runtime-разрешений при установке, повторяем без `-g`.".to_string(),
            retry_with_downgrade: false,
            retry_with_force_sdk: false,
            retry_with_bypass_low_sdk: false,
            retry_without_grant: true,
        };
    }

    PmInstallDiagnosis {
        error_code,
        summary_ru: format!("Ошибка установки Android PackageManager: {}", output.trim()),
        retry_with_downgrade: false,
        retry_with_force_sdk: false,
        retry_with_bypass_low_sdk: false,
        retry_without_grant: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_split_tag_variants() {
        assert_eq!(classify_split_tag("base.apk"), SplitKind::Base);
        assert_eq!(classify_split_tag("base-master.apk"), SplitKind::Base);
        assert_eq!(
            classify_split_tag("split_config.arm64_v8a.apk"),
            SplitKind::Abi("arm64-v8a".to_string())
        );
        assert_eq!(
            classify_split_tag("config.x86_64"),
            SplitKind::Abi("x86_64".to_string())
        );
        assert_eq!(
            classify_split_tag("split_config.xxhdpi.apk"),
            SplitKind::Density("xxhdpi".to_string())
        );
        assert_eq!(
            classify_split_tag("split_config.ru.apk"),
            SplitKind::Language("ru".to_string())
        );
        assert_eq!(
            classify_split_tag("split_config.pt_br.apk"),
            SplitKind::Language("pt_br".to_string())
        );
        assert_eq!(
            classify_split_tag("dynamic_feature.apk"),
            SplitKind::Feature("dynamic_feature".to_string())
        );
    }

    #[test]
    fn test_merge_prop_package_list_respects_91_char_limit() {
        let mut list = String::new();
        for i in 0..8 {
            let pkg = format!("com.example.longpackagename.app{:02}", i);
            list = merge_prop_package_list(&list, &pkg, true, 91);
            assert!(list.len() <= 91, "len {} exceeded 91", list.len());
            assert!(list.starts_with(&pkg), "most recent app must fit first");
        }
        let removed = merge_prop_package_list("com.a,com.b,com.c", "com.b", false, 91);
        assert_eq!(removed, "com.a,com.c");
        assert_eq!(merge_prop_package_list("*", "com.a", true, 91), "*");
    }

    #[test]
    fn test_resolve_window_dimensions_clamps_to_laptop_screen() {
        let mut profile = AdaptationProfile::default();
        profile.window_mode = WindowMode::PhonePortrait;
        let dims = resolve_window_dimensions(&profile, 1324, 688).unwrap();
        assert!(dims.0 <= 1324 && dims.1 <= 688, "got {:?}", dims);
        assert!(dims.1 > dims.0, "portrait must remain taller than wide: {:?}", dims);

        profile.window_mode = WindowMode::CustomSize;
        profile.width = Some(1920);
        profile.height = Some(1080);
        let scaled = resolve_window_dimensions(&profile, 1324, 688).unwrap();
        assert!(scaled.0 <= 1324 && scaled.1 <= 688, "got {:?}", scaled);
    }

    #[test]
    fn test_classify_pm_install_failures() {
        let dg = classify_pm_install_failure("Failure [INSTALL_FAILED_VERSION_DOWNGRADE]");
        assert!(dg.retry_with_downgrade);
        assert_eq!(dg.error_code.as_deref(), Some("INSTALL_FAILED_VERSION_DOWNGRADE"));

        let sdk = classify_pm_install_failure(
            "Failure [INSTALL_FAILED_DEPRECATED_SDK_VERSION: App package must target at least SDK version 23]",
        );
        assert!(sdk.retry_with_force_sdk);
        assert!(sdk.retry_with_bypass_low_sdk);

        let abi = classify_pm_install_failure("Failure [INSTALL_FAILED_NO_MATCHING_ABIS: Failed to extract native libraries]");
        assert!(!abi.retry_with_downgrade);
        assert!(abi.summary_ru.contains("setup-arm"));
    }
}
