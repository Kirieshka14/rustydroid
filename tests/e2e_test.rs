//! Comprehensive Opaque-Box E2E Test Suite for RustyDroid
//!
//! Covering all 28 features in PROJECT.md across 5 tiers:
//! - Tier 1: Feature Coverage (representative happy-path tests for Features 1-26)
//! - Tier 2: Boundary & Corner Cases (limits, empty inputs, malformed AXML, missing sockets, dead sessions)
//! - Tier 3: Cross-Feature Combinations (pairwise interactions: APK parse -> desktop entry -> icon; status -> init -> run)
//! - Tier 4: Real-World Application Scenarios (realistic workloads: F-Droid inspection, Aurora Store desktop registration, system health diagnostics)
//! - Tier 5: Adversarial Coverage Hardening (Feature 28: chunk overflow, unknown chunks, special characters, corrupted files)

#![allow(unused_imports, unused_variables)]

mod common;

#[path = "e2e/tier1_feature_coverage.rs"]
mod tier1_feature_coverage;

#[path = "e2e/tier2_boundary_corner_cases.rs"]
mod tier2_boundary_corner_cases;

#[path = "e2e/tier3_cross_feature_combinations.rs"]
mod tier3_cross_feature_combinations;

#[path = "e2e/tier4_real_world_scenarios.rs"]
mod tier4_real_world_scenarios;

#[path = "e2e/tier5_adversarial_hardening.rs"]
mod tier5_adversarial_hardening;
