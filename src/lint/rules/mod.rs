//! One file per rule, named after the rule id with `/` and `-` replaced by `_`.
//!
//! This registry is complete: rule tasks fill in their own file and never edit this one.

use super::Rule;

pub mod hygiene_autorev;
pub mod hygiene_continuation;
pub mod hygiene_git_pv;
pub mod hygiene_host_path;
pub mod hygiene_insane_skip;
pub mod hygiene_missing_metadata;
pub mod hygiene_parse_error;
pub mod hygiene_release_removed;
pub mod hygiene_release_syntax;
pub mod hygiene_src_uri_checksum;
pub mod hygiene_tabs;
pub mod hygiene_variable_order;
pub mod hygiene_workdir_s;
pub mod org_append_missing_filesextrapaths;
pub mod org_append_no_match;
pub mod org_append_wildcard_broad;
pub mod org_class_export_functions;
pub mod org_class_location;
pub mod org_class_recipe_specific;
pub mod org_duplicate_versions;
pub mod org_generated_conf_edit;
pub mod org_inc_has_version_data;
pub mod org_inc_unused;
pub mod org_include_must_exist;
pub mod org_inherit_defer;
pub mod org_layer_conf;
pub mod org_policy_in_recipe;
pub mod org_recipe_change_in_local_conf;
pub mod org_recipe_path;
pub mod org_repeated_logic;
pub mod org_upstream_edit;
pub mod override_append_no_space;
pub mod override_append_vs_plus_equals;
pub mod override_dead;
pub mod override_filesextrapaths;
pub mod override_legacy_syntax;
pub mod override_missing_package;
pub mod override_plus_equals_on_default;
pub mod override_plus_equals_with_override;
pub mod override_remove_in_recipe;
pub mod override_shadowed;
pub mod override_weak_after_default;

/// Every rule, in catalogue order (docs/lint-rules.md).
pub static ALL: &[&dyn Rule] = &[
    &override_legacy_syntax::OverrideLegacySyntax,
    &override_append_no_space::OverrideAppendNoSpace,
    &override_plus_equals_with_override::OverridePlusEqualsWithOverride,
    &override_remove_in_recipe::OverrideRemoveInRecipe,
    &override_dead::OverrideDead,
    &override_shadowed::OverrideShadowed,
    &override_weak_after_default::OverrideWeakAfterDefault,
    &override_missing_package::OverrideMissingPackage,
    &override_append_vs_plus_equals::OverrideAppendVsPlusEquals,
    &override_plus_equals_on_default::OverridePlusEqualsOnDefault,
    &override_filesextrapaths::OverrideFilesextrapaths,
    &org_recipe_path::OrgRecipePath,
    &org_duplicate_versions::OrgDuplicateVersions,
    &org_inc_has_version_data::OrgIncHasVersionData,
    &org_inc_unused::OrgIncUnused,
    &org_include_must_exist::OrgIncludeMustExist,
    &org_repeated_logic::OrgRepeatedLogic,
    &org_class_recipe_specific::OrgClassRecipeSpecific,
    &org_class_export_functions::OrgClassExportFunctions,
    &org_class_location::OrgClassLocation,
    &org_inherit_defer::OrgInheritDefer,
    &org_append_no_match::OrgAppendNoMatch,
    &org_append_wildcard_broad::OrgAppendWildcardBroad,
    &org_append_missing_filesextrapaths::OrgAppendMissingFilesextrapaths,
    &org_policy_in_recipe::OrgPolicyInRecipe,
    &org_recipe_change_in_local_conf::OrgRecipeChangeInLocalConf,
    &org_generated_conf_edit::OrgGeneratedConfEdit,
    &org_layer_conf::OrgLayerConf,
    &org_upstream_edit::OrgUpstreamEdit,
    &hygiene_missing_metadata::HygieneMissingMetadata,
    &hygiene_git_pv::HygieneGitPv,
    &hygiene_insane_skip::HygieneInsaneSkip,
    &hygiene_autorev::HygieneAutorev,
    &hygiene_src_uri_checksum::HygieneSrcUriChecksum,
    &hygiene_host_path::HygieneHostPath,
    &hygiene_workdir_s::HygieneWorkdirS,
    &hygiene_variable_order::HygieneVariableOrder,
    &hygiene_tabs::HygieneTabs,
    &hygiene_continuation::HygieneContinuation,
    &hygiene_parse_error::HygieneParseError,
    &hygiene_release_syntax::HygieneReleaseSyntax,
    &hygiene_release_removed::HygieneReleaseRemoved,
];
