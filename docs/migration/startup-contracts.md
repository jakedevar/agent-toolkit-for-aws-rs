# startup-contracts (#4409)

Programme #4369, slice `startup-contracts`. Rust replacements for the cross-file,
workflow and fixture contract tests in `plugins/aws-startup-advisor/tests/`. The Python
files stay in place as read-only evidence (retirement is #4415). Nothing here runs Python.

## Inventory and coverage

37 inventory rows belong to the slice (36 `tests/test_*.py` files and the data they read).
Checked with `git log -S` and the Rust test names before writing anything:

| Python test file | Status |
| --- | --- |
| `test_validate_artifacts.py` (61), `test_model_id_lint.py` (11), `test_emit_plan_json.py` (8), `test_consent_disclosure_audit.py` (39) | already ported by #4399 (`startup/artifacts/tests.rs` and `tests/documents.rs`, `startup/tests.rs`, `startup/plan/tests.rs`, `startup/consent/tests.rs`); see `docs/migration/startup-validators.md` |
| `test_azure_clarify_fast_path_contracts.py` goldens and Q-D2 boundary (`test_clarify_goldens_pass`, `test_db_cutover_rule_boundary`) | the three clarify goldens run through `toolkit startup check-expected azure-clarify*` and are asserted, with failing neighbours, by `startup/fixtures/tests/azure.rs` (#4408, merged here from `e54d43ee`); the seven Q-D2 cases are asserted by `fixtures/azure/clarify.rs::unit::the_q_d2_rule_has_an_inclusive_100_gib_boundary`, which gained the three cases #4408 lacked (64 GiB, no server, 64 GiB + unmeasured) |
| `test_ai_capability_catalog_contracts.py` (12) | ported in-crate, table below |
| the 26 markdown/workflow files in the table below (196 tests) | ported here |
| `test_ai_migration_matrix_contracts.py` (21), `test_ai_workload_profile_contracts.py` (15), `test_aws_design_ai_contracts.py` (23), `test_estimation_ai_contracts.py` (19), `test_generation_ai_contracts.py` (17), `test_azure_rdfa_discover_contracts.py` (20) | ported by #4806 (115 test functions; the split issue counted 107), table "schema-driven AI contracts" below. The fixtures run through `startup/artifacts/tests/{matrix,workload,design_ai,estimation_ai,generation_ai,rdfa}.rs` and the shared `support.rs` (the pattern of `catalog.rs`); the RDfA prose contracts and helper simulations are `tests/startup_contracts/rdfa.rs` |

## Design

`crates/toolkit-cli/tests/startup_contracts/` is one integration-test target.

* `common.rs`: `Plugin` is a view of `plugins/aws-startup-advisor`. `Plugin::real()` reads the
  committed files; `with_edit` shadows one file with an edited copy written into a private
  temporary directory, so a contract can be run against a broken copy without touching the tree.
  `check(contract, &[(edit, reason)])` requires the contract to pass on the committed plugin and to
  fail, for the named reason, on each edited copy. An edit that changes nothing panics, so a
  mutation can not be vacuous.
* `rules.rs`: most contracts say "this region of this file contains X and not Y". A `Rule`
  states that (file, region with Python slicing semantics, whitespace-normalised or case-folded
  matching as the Python did, `all`/`any`/`none` text, and `before(a, b)` ordering).
  `check_rules` runs the rules on the committed plugin and then **derives one broken copy per
  assertion** (the required text replaced inside its region, the forbidden text inserted into it,
  the ordering reversed) and requires the rules to fail for exactly that reason. Every ported
  substring assertion is therefore shown to be non-vacuous.
* Python `text.index(...)` semantics are kept: a missing anchor fails, slices run between the
  first occurrences, windows count characters. The few places where the port is knowingly
  looser or stricter are listed under "Intentional differences".
* Fixture and plugin content is read as committed and is byte-identical; nothing is rewritten.

Run: `cargo test -p toolkit-cli --test startup_contracts` (the daemon job `workspace-test` runs it).

## One-to-one mapping (markdown, workflow and cross-file contracts)

| Python file | Python test | Rust test |
| --- | --- | --- |
| `test_ai_only_option_c_framework_route` | `test_absent_migration_path_consults_framework_before_adapter` | `startup_contracts::option_c::test_absent_migration_path_consults_framework_before_adapter` |
| `test_ai_only_option_c_framework_route` | `test_dd38bc2_spellings_and_openrouter_priority_preserved` | `startup_contracts::option_c::test_dd38bc2_spellings_and_openrouter_priority_preserved` |
| `test_ai_only_option_c_framework_route` | `test_framework_consult_mirrors_generate_dispatch` | `startup_contracts::option_c::test_framework_consult_mirrors_generate_dispatch` |
| `test_azure_clarify_fast_path_contracts` | `test_clarify_gates_are_mode_aware` | `startup_contracts::azure_clarify::test_clarify_gates_are_mode_aware` |
| `test_azure_clarify_fast_path_contracts` | `test_db_cutover_unknown_size_branch` | `startup_contracts::azure_clarify::test_db_cutover_unknown_size_branch` |
| `test_azure_clarify_fast_path_contracts` | `test_gate_mutations_reconcile_scenarios` | `startup_contracts::azure_clarify::test_gate_mutations_reconcile_scenarios` |
| `test_azure_clarify_fast_path_contracts` | `test_step3b_clears_all_deferral_state` | `startup_contracts::azure_clarify::test_step3b_clears_all_deferral_state` |
| `test_azure_clarify_fast_path_contracts` | `test_workshop_patch_updates_default_index` | `startup_contracts::azure_clarify::test_workshop_patch_updates_default_index` |
| `test_azure_discover_ordering` | `test_shared_init_precedes_both_pre_dispatch_source_actions` | `startup_contracts::discover::test_shared_init_precedes_both_pre_dispatch_source_actions` |
| `test_azure_discover_ordering` | `test_shared_init_selects_or_terminates_run_before_pre_actions` | `startup_contracts::discover::test_shared_init_selects_or_terminates_run_before_pre_actions` |
| `test_azure_discover_ordering` | `test_usage_api_sections_no_longer_under_pre_dispatch_heading` | `startup_contracts::discover::test_usage_api_sections_no_longer_under_pre_dispatch_heading` |
| `test_azure_discover_ordering` | `test_usage_api_sections_run_after_phase_execution_step` | `startup_contracts::discover::test_usage_api_sections_run_after_phase_execution_step` |
| `test_azure_discovery_source_guidance` | `test_plugin_readme_names_rdfa_for_azure_only` | `startup_contracts::guides::test_plugin_readme_names_rdfa_for_azure_only` |
| `test_azure_discovery_source_guidance` | `test_skill_summary_names_rdfa_and_not_billing_exports` | `startup_contracts::guides::test_skill_summary_names_rdfa_and_not_billing_exports` |
| `test_cost_completeness` | `test_anthropic_discover_copies_remain_mirrored_for_cost_status_text` | `startup_contracts::cost_completeness::test_anthropic_discover_copies_remain_mirrored_for_cost_status_text` |
| `test_cost_completeness` | `test_both_anthropic_discover_copies_detect_priority_tier_rows` | `startup_contracts::cost_completeness::test_both_anthropic_discover_copies_detect_priority_tier_rows` |
| `test_cost_completeness` | `test_both_anthropic_discover_copies_document_cost_status_enum` | `startup_contracts::cost_completeness::test_both_anthropic_discover_copies_document_cost_status_enum` |
| `test_cost_completeness` | `test_both_anthropic_discover_copies_document_cost_unavailable_null_semantics` | `startup_contracts::cost_completeness::test_both_anthropic_discover_copies_document_cost_unavailable_null_semantics` |
| `test_cost_completeness` | `test_both_anthropic_discover_copies_do_not_mark_priority_tier_as_complete` | `startup_contracts::cost_completeness::test_both_anthropic_discover_copies_do_not_mark_priority_tier_as_complete` |
| `test_cost_completeness` | `test_both_anthropic_discover_copies_error_handling_table_sets_cost_unavailable` | `startup_contracts::cost_completeness::test_both_anthropic_discover_copies_error_handling_table_sets_cost_unavailable` |
| `test_cost_completeness` | `test_both_estimate_ai_files_exclude_cost_unavailable_dollars_from_part_1` | `startup_contracts::cost_completeness::test_both_estimate_ai_files_exclude_cost_unavailable_dollars_from_part_1` |
| `test_cost_completeness` | `test_both_estimate_ai_files_still_count_cost_unavailable_volume_in_part_2` | `startup_contracts::cost_completeness::test_both_estimate_ai_files_still_count_cost_unavailable_volume_in_part_2` |
| `test_cost_completeness` | `test_both_openrouter_discover_copies_document_real_cost_provenance_detection` | `startup_contracts::cost_completeness::test_both_openrouter_discover_copies_document_real_cost_provenance_detection` |
| `test_cost_completeness` | `test_both_openrouter_discover_copies_have_fourth_analytics_table_row` | `startup_contracts::cost_completeness::test_both_openrouter_discover_copies_have_fourth_analytics_table_row` |
| `test_cost_completeness` | `test_openrouter_discover_copies_remain_mirrored_for_cost_provenance_text` | `startup_contracts::cost_completeness::test_openrouter_discover_copies_remain_mirrored_for_cost_provenance_text` |
| `test_cost_completeness` | `test_report_generator_documents_cost_unavailable_presentation_rule` | `startup_contracts::cost_completeness::test_report_generator_documents_cost_unavailable_presentation_rule` |
| `test_cost_completeness` | `test_skill_md_step15_consumes_per_model_cost_provenance` | `startup_contracts::cost_completeness::test_skill_md_step15_consumes_per_model_cost_provenance` |
| `test_cost_completeness` | `test_skill_md_sum_step_excludes_cost_unavailable_dollars` | `startup_contracts::cost_completeness::test_skill_md_sum_step_excludes_cost_unavailable_dollars` |
| `test_cost_completeness` | `test_skill_md_sum_step_has_byok_overlap_exception` | `startup_contracts::cost_completeness::test_skill_md_sum_step_has_byok_overlap_exception` |
| `test_cost_completeness` | `test_skill_md_usage_baseline_schema_has_cost_status_per_window` | `startup_contracts::cost_completeness::test_skill_md_usage_baseline_schema_has_cost_status_per_window` |
| `test_credential_lifecycle` | `test_anthropic_all_rows_failed_deletes_key_file` | `startup_contracts::credentials::test_anthropic_all_rows_failed_deletes_key_file` |
| `test_credential_lifecycle` | `test_anthropic_key_invalid_mid_run_keeps_key_file` | `startup_contracts::credentials::test_anthropic_key_invalid_mid_run_keeps_key_file` |
| `test_credential_lifecycle` | `test_anthropic_org_type_fork_deletes_key_file` | `startup_contracts::credentials::test_anthropic_org_type_fork_deletes_key_file` |
| `test_credential_lifecycle` | `test_azure_openai_all_usage_failed_deletes_key_file` | `startup_contracts::credentials::test_azure_openai_all_usage_failed_deletes_key_file` |
| `test_credential_lifecycle` | `test_azure_openai_key_invalid_mid_run_keeps_key_file` | `startup_contracts::credentials::test_azure_openai_key_invalid_mid_run_keeps_key_file` |
| `test_credential_lifecycle` | `test_key_invalid_mid_run_is_the_only_documented_retry_carve_out` | `startup_contracts::credentials::test_key_invalid_mid_run_is_the_only_documented_retry_carve_out` |
| `test_credential_lifecycle` | `test_manifest_schema_declares_retry_pending` | `startup_contracts::credentials::test_manifest_schema_declares_retry_pending` |
| `test_credential_lifecycle` | `test_step4_success_path_deletion_language_is_unchanged` | `startup_contracts::credentials::test_step4_success_path_deletion_language_is_unchanged` |
| `test_decision_gate_wiring` | `test_estimate_exports_only_after_postconditions` | `startup_contracts::discover::test_estimate_exports_only_after_postconditions` |
| `test_decision_gate_wiring` | `test_handoff_exits_are_fail_open_gated` | `startup_contracts::discover::test_handoff_exits_are_fail_open_gated` |
| `test_decision_gate_wiring` | `test_handoff_exits_invoke_plan_writer` | `startup_contracts::discover::test_handoff_exits_invoke_plan_writer` |
| `test_decision_gate_wiring` | `test_handoff_exits_present_import_cta` | `startup_contracts::discover::test_handoff_exits_present_import_cta` |
| `test_estimate_assemble_stale_artifacts` | `test_azure_estimate_assemble_invalidates_stale_sibling_route_artifact` | `startup_contracts::guides::test_azure_estimate_assemble_invalidates_stale_sibling_route_artifact` |
| `test_estimate_clarify_volume_resolution` | `test_anthropic_discover_copies_remain_mirrored_after_this_fix` | `startup_contracts::ai_phases::test_anthropic_discover_copies_remain_mirrored_after_this_fix` |
| `test_estimate_clarify_volume_resolution` | `test_azure_and_gcp_estimate_ai_partial_provider_exception_match_in_substance` | `startup_contracts::ai_phases::test_azure_and_gcp_estimate_ai_partial_provider_exception_match_in_substance` |
| `test_estimate_clarify_volume_resolution` | `test_azure_estimate_ai_has_ported_unresolved_scenario_rows_language` | `startup_contracts::ai_phases::test_azure_estimate_ai_has_ported_unresolved_scenario_rows_language` |
| `test_estimate_clarify_volume_resolution` | `test_azure_estimate_ai_no_longer_unconditionally_directs_to_tier_table` | `startup_contracts::ai_phases::test_azure_estimate_ai_no_longer_unconditionally_directs_to_tier_table` |
| `test_estimate_clarify_volume_resolution` | `test_clarify_q3_falls_through_to_asking_normally_when_all_contributors_partial` | `startup_contracts::ai_phases::test_clarify_q3_falls_through_to_asking_normally_when_all_contributors_partial` |
| `test_estimate_clarify_volume_resolution` | `test_clarify_q3_first_branch_checks_provenance_before_auto_resolving` | `startup_contracts::ai_phases::test_clarify_q3_first_branch_checks_provenance_before_auto_resolving` |
| `test_estimate_clarify_volume_resolution` | `test_discover_merge_stamps_partial_window_and_cost_status_onto_current_costs` | `startup_contracts::ai_phases::test_discover_merge_stamps_partial_window_and_cost_status_onto_current_costs` |
| `test_estimate_option_a_single_close` | `test_estimate_md_step_4_is_the_only_close_owner` | `startup_contracts::guides::test_estimate_md_step_4_is_the_only_close_owner` |
| `test_estimate_option_a_single_close` | `test_step_3a_does_not_emit_its_own_close` | `startup_contracts::guides::test_step_3a_does_not_emit_its_own_close` |
| `test_gcp_design_guide_selection` | `test_both_branch_no_longer_hardcodes_gemini_and_openai_only` | `startup_contracts::guides::test_both_branch_no_longer_hardcodes_gemini_and_openai_only` |
| `test_gcp_design_guide_selection` | `test_both_branch_selects_guides_from_actual_provider_presence` | `startup_contracts::guides::test_both_branch_selects_guides_from_actual_provider_presence` |
| `test_gemini_guide_body_parity` | `test_both_gemini_guides_exist` | `startup_contracts::guides::test_both_gemini_guides_exist` |
| `test_gemini_guide_body_parity` | `test_gemini_guide_bodies_are_byte_identical` | `startup_contracts::guides::test_gemini_guide_bodies_are_byte_identical` |
| `test_generate_mantle_dispatch` | `test_azure_generate_mantle_branch_still_correct` | `startup_contracts::ai_phases::test_azure_generate_mantle_branch_still_correct` |
| `test_generate_mantle_dispatch` | `test_step1m_base_url_uses_openai_v1_with_caveat` | `startup_contracts::ai_phases::test_step1m_base_url_uses_openai_v1_with_caveat` |
| `test_generate_mantle_dispatch` | `test_step2_harness_import_conditional_on_mantle_path` | `startup_contracts::ai_phases::test_step2_harness_import_conditional_on_mantle_path` |
| `test_generate_mantle_dispatch` | `test_step3_mantle_iam_and_test_api_are_conditional` | `startup_contracts::ai_phases::test_step3_mantle_iam_and_test_api_are_conditional` |
| `test_gpt6_review_round2` | `test_both_estimate_ai_files_require_matching_context_tiers` | `startup_contracts::gpt6::test_both_estimate_ai_files_require_matching_context_tiers` |
| `test_gpt6_review_round2` | `test_both_pricing_caches_document_gpt6_breakpoint` | `startup_contracts::gpt6::test_both_pricing_caches_document_gpt6_breakpoint` |
| `test_gpt6_review_round2` | `test_design_ai_scopes_mantle_only_to_gpt55_gpt54` | `startup_contracts::gpt6::test_design_ai_scopes_mantle_only_to_gpt55_gpt54` |
| `test_gpt6_review_round2` | `test_generate_ai_gcp_timeline_handles_runtime_openai_cris` | `startup_contracts::gpt6::test_generate_ai_gcp_timeline_handles_runtime_openai_cris` |
| `test_gpt6_review_round2` | `test_generate_artifacts_ai_dispatches_runtime_openai_cris` | `startup_contracts::gpt6::test_generate_artifacts_ai_dispatches_runtime_openai_cris` |
| `test_gpt6_review_round2` | `test_openai_on_bedrock_notes_mantle_never_supports_invoke` | `startup_contracts::gpt6::test_openai_on_bedrock_notes_mantle_never_supports_invoke` |
| `test_gpt6_review_round2` | `test_openai_on_bedrock_scopes_invoke_support_per_model` | `startup_contracts::gpt6::test_openai_on_bedrock_scopes_invoke_support_per_model` |
| `test_gpt6_review_round2` | `test_schema_design_aws_ai_vocabulary_includes_runtime_openai_cris` | `startup_contracts::gpt6::test_schema_design_aws_ai_vocabulary_includes_runtime_openai_cris` |
| `test_gpt6_review_round5` | `test_behavior_delta_no_longer_claims_chat_completions_unverified` | `startup_contracts::gpt6::test_behavior_delta_no_longer_claims_chat_completions_unverified` |
| `test_gpt6_review_round5` | `test_both_design_ai_no_longer_force_gpt6_chat_completions_reshape` | `startup_contracts::gpt6::test_both_design_ai_no_longer_force_gpt6_chat_completions_reshape` |
| `test_gpt6_review_round5` | `test_both_design_ai_require_converse_client_for_astra_guardrails` | `startup_contracts::gpt6::test_both_design_ai_require_converse_client_for_astra_guardrails` |
| `test_gpt6_review_round5` | `test_both_generate_artifacts_ai_consume_required_client_api_in_cris_step` | `startup_contracts::gpt6::test_both_generate_artifacts_ai_consume_required_client_api_in_cris_step` |
| `test_gpt6_review_round5` | `test_both_generate_artifacts_ai_document_gpt6_cris_project_default` | `startup_contracts::gpt6::test_both_generate_artifacts_ai_document_gpt6_cris_project_default` |
| `test_gpt6_review_round5` | `test_gcp_generate_ai_no_longer_forces_gpt6_chat_completions_reshape` | `startup_contracts::gpt6::test_gcp_generate_ai_no_longer_forces_gpt6_chat_completions_reshape` |
| `test_gpt6_review_round5` | `test_region_only_cris_move_keeps_responses` | `startup_contracts::gpt6::test_region_only_cris_move_keeps_responses` |
| `test_gpt6_review_round5` | `test_schema_documents_required_client_api_with_converse_for_guardrails` | `startup_contracts::gpt6::test_schema_documents_required_client_api_with_converse_for_guardrails` |
| `test_gpt6_round4_findings` | `test_azure_design_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris` | `startup_contracts::gpt6_round4::test_azure_design_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris` |
| `test_gpt6_round4_findings` | `test_azure_generate_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris` | `startup_contracts::gpt6_round4::test_azure_generate_assert_scopes_gpt_ban_to_55_54_and_carves_out_cris` |
| `test_gpt6_round4_findings` | `test_behavior_delta_reference_covers_gpt6` | `startup_contracts::gpt6_round4::test_behavior_delta_reference_covers_gpt6` |
| `test_gpt6_round4_findings` | `test_both_estimators_carve_out_global_cris_parity` | `startup_contracts::gpt6_round4::test_both_estimators_carve_out_global_cris_parity` |
| `test_gpt6_round4_findings` | `test_code_analyzer_covers_gpt6` | `startup_contracts::gpt6_round4::test_code_analyzer_covers_gpt6` |
| `test_gpt6_round4_findings` | `test_code_rewriter_covers_gpt6` | `startup_contracts::gpt6_round4::test_code_rewriter_covers_gpt6` |
| `test_gpt6_round4_findings` | `test_gcp_generate_ai_routes_cris_through_bedrock_runtime_not_mantle` | `startup_contracts::gpt6_round4::test_gcp_generate_ai_routes_cris_through_bedrock_runtime_not_mantle` |
| `test_gpt6_round4_findings` | `test_gcp_step1m_and_harness_agree_on_adapter_free_paths` | `startup_contracts::gpt6_round4::test_gcp_step1m_and_harness_agree_on_adapter_free_paths` |
| `test_gpt6_round4_findings` | `test_guardrails_canonical_and_vendored_name_gpt6_family` | `startup_contracts::gpt6_round4::test_guardrails_canonical_and_vendored_name_gpt6_family` |
| `test_gpt6_round4_findings` | `test_openai_guide_canonical_and_vendored_region_gate_names_gpt6` | `startup_contracts::gpt6_round4::test_openai_guide_canonical_and_vendored_region_gate_names_gpt6` |
| `test_gpt6_round4_findings` | `test_prompt_evaluator_covers_gpt6` | `startup_contracts::gpt6_round4::test_prompt_evaluator_covers_gpt6` |
| `test_gpt6_round4_findings` | `test_report_generator_section_6_2_has_gpt6_source_rows` | `startup_contracts::gpt6_round4::test_report_generator_section_6_2_has_gpt6_source_rows` |
| `test_gpt6_round4_findings` | `test_resolver_covers_bare_gpt6_ids` | `startup_contracts::gpt6_round4::test_resolver_covers_bare_gpt6_ids` |
| `test_gpt6_source_breakpoint` | `test_exact_boundary_272000_input_tokens_stays_short_context` | `startup_contracts::cost_compare::test_exact_boundary_272000_input_tokens_stays_short_context` |
| `test_gpt6_source_breakpoint` | `test_exact_boundary_272001_input_tokens_reprices_whole_request` | `startup_contracts::cost_compare::test_exact_boundary_272001_input_tokens_reprices_whole_request` |
| `test_gpt6_source_breakpoint` | `test_gpt6_sol_300k_input_crosses_breakpoint_on_both_sides` | `startup_contracts::cost_compare::test_gpt6_sol_300k_input_crosses_breakpoint_on_both_sides` |
| `test_gpt6_source_breakpoint` | `test_mixed_tier_distribution_partitions_before_pricing` | `startup_contracts::cost_compare::test_mixed_tier_distribution_partitions_before_pricing` |
| `test_gpt6_source_breakpoint` | `test_pair_without_per_request_tokens_keeps_flat_rate_unchanged` | `startup_contracts::cost_compare::test_pair_without_per_request_tokens_keeps_flat_rate_unchanged` |
| `test_gpt6_source_breakpoint` | `test_source_breakpoint_independent_of_flat_target` | `startup_contracts::cost_compare::test_source_breakpoint_independent_of_flat_target` |
| `test_gpt6_source_breakpoint` | `test_tier_distribution_absent_falls_back_to_existing_behavior` | `startup_contracts::cost_compare::test_tier_distribution_absent_falls_back_to_existing_behavior` |
| `test_heroku_discover_addon_identity` | `test_discover_assemble_rule_1a_pairs_only_when_unambiguous` | `startup_contracts::heroku_addon::test_discover_assemble_rule_1a_pairs_only_when_unambiguous` |
| `test_heroku_discover_addon_identity` | `test_discover_assemble_rule_1b_reports_ambiguous_addon_groups` | `startup_contracts::heroku_addon::test_discover_assemble_rule_1b_reports_ambiguous_addon_groups` |
| `test_heroku_discover_addon_identity` | `test_discover_live_resource_entry_includes_addon_name_discriminator` | `startup_contracts::heroku_addon::test_discover_live_resource_entry_includes_addon_name_discriminator` |
| `test_heroku_discover_addon_identity` | `test_discover_terraform_resource_id_and_attribute_table_include_addon_name` | `startup_contracts::heroku_addon::test_discover_terraform_resource_id_and_attribute_table_include_addon_name` |
| `test_heroku_discover_addon_identity` | `test_schema_addon_name_is_required_config_field` | `startup_contracts::heroku_addon::test_schema_addon_name_is_required_config_field` |
| `test_heroku_discover_addon_identity` | `test_schema_resource_id_format_includes_addon_name` | `startup_contracts::heroku_addon::test_schema_resource_id_format_includes_addon_name` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_cost_engine_part1b_computes_deferred_resource_cost` | `startup_contracts::heroku_deferred::test_cost_engine_part1b_computes_deferred_resource_cost` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_cost_engine_part3_defines_comparison_status_and_keeps_full_baseline` | `startup_contracts::heroku_deferred::test_cost_engine_part3_defines_comparison_status_and_keeps_full_baseline` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_cost_engine_part8_soft_trigger_table_has_comparison_status_row` | `startup_contracts::heroku_deferred::test_cost_engine_part8_soft_trigger_table_has_comparison_status_row` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_design_mapping_deferred_entry_carries_resource_id` | `startup_contracts::heroku_deferred::test_design_mapping_deferred_entry_carries_resource_id` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_estimate_assemble_financial_summary_propagates_comparison_status` | `startup_contracts::heroku_deferred::test_estimate_assemble_financial_summary_propagates_comparison_status` |
| `test_heroku_estimate_deferred_cost_comparison` | `test_generate_report_renders_unmatched_deferred_caveat_in_exec_costs` | `startup_contracts::heroku_deferred::test_generate_report_renders_unmatched_deferred_caveat_in_exec_costs` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_marks_advanced_tier_unpriced_when_pool_data_missing` | `startup_contracts::heroku_pricing::test_cost_engine_marks_advanced_tier_unpriced_when_pool_data_missing` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung2_covers_private_and_shield_advanced_variants` | `startup_contracts::heroku_pricing::test_cost_engine_rung2_covers_private_and_shield_advanced_variants` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung2_detects_advanced_family_by_identity_not_tier` | `startup_contracts::heroku_pricing::test_cost_engine_rung2_detects_advanced_family_by_identity_not_tier` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung2_marks_failed_capture_record_unpriced_not_zero` | `startup_contracts::heroku_pricing::test_cost_engine_rung2_marks_failed_capture_record_unpriced_not_zero` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung2_prices_advanced_tier_via_pool_sum_not_flat_price` | `startup_contracts::heroku_pricing::test_cost_engine_rung2_prices_advanced_tier_via_pool_sum_not_flat_price` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung3_covers_failed_capture_and_private_shield` | `startup_contracts::heroku_pricing::test_cost_engine_rung3_covers_failed_capture_and_private_shield` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung3_detects_advanced_family_by_identity_not_tier` | `startup_contracts::heroku_pricing::test_cost_engine_rung3_detects_advanced_family_by_identity_not_tier` |
| `test_heroku_postgres_advanced_pricing` | `test_cost_engine_rung3_references_advanced_tier_formula` | `startup_contracts::heroku_pricing::test_cost_engine_rung3_references_advanced_tier_formula` |
| `test_heroku_postgres_advanced_pricing` | `test_discover_live_associates_each_database_by_addon_name_not_just_app` | `startup_contracts::heroku_pricing::test_discover_live_associates_each_database_by_addon_name_not_just_app` |
| `test_heroku_postgres_advanced_pricing` | `test_discover_live_capture_probes_both_postgres_tiers` | `startup_contracts::heroku_pricing::test_discover_live_capture_probes_both_postgres_tiers` |
| `test_heroku_postgres_advanced_pricing` | `test_discover_live_capture_row_7b_passes_explicit_database_identifier` | `startup_contracts::heroku_pricing::test_discover_live_capture_row_7b_passes_explicit_database_identifier` |
| `test_heroku_postgres_advanced_pricing` | `test_discover_live_parses_advanced_pool_blocks` | `startup_contracts::heroku_pricing::test_discover_live_parses_advanced_pool_blocks` |
| `test_heroku_postgres_advanced_pricing` | `test_generate_report_requires_rendering_baseline_note` | `startup_contracts::heroku_pricing::test_generate_report_requires_rendering_baseline_note` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_advanced_tier_documents_multi_pool_known_limitation` | `startup_contracts::heroku_pricing::test_pricing_cache_advanced_tier_documents_multi_pool_known_limitation` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_advanced_tier_lists_all_eleven_levels` | `startup_contracts::heroku_pricing::test_pricing_cache_advanced_tier_lists_all_eleven_levels` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_advanced_tier_marks_unconfirmed_rates_unverified` | `startup_contracts::heroku_pricing::test_pricing_cache_advanced_tier_marks_unconfirmed_rates_unverified` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_advanced_tier_states_storage_overage_rate` | `startup_contracts::heroku_pricing::test_pricing_cache_advanced_tier_states_storage_overage_rate` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_classic_tier_rows_untouched` | `startup_contracts::heroku_pricing::test_pricing_cache_classic_tier_rows_untouched` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_has_advanced_tier_heading` | `startup_contracts::heroku_pricing::test_pricing_cache_has_advanced_tier_heading` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_rule9_gates_on_identity_not_tier` | `startup_contracts::heroku_pricing::test_pricing_cache_rule9_gates_on_identity_not_tier` |
| `test_heroku_postgres_advanced_pricing` | `test_pricing_cache_usage_rules_document_advanced_formula` | `startup_contracts::heroku_pricing::test_pricing_cache_usage_rules_document_advanced_formula` |
| `test_heroku_postgres_advanced_pricing` | `test_schema_documents_advanced_tier_fields` | `startup_contracts::heroku_pricing::test_schema_documents_advanced_tier_fields` |
| `test_llm_to_bedrock_run_state` | `test_a3_heading_is_named_consistently_and_performs_the_write` | `startup_contracts::run_state::test_a3_heading_is_named_consistently_and_performs_the_write` |
| `test_llm_to_bedrock_run_state` | `test_context_block_includes_usage_baseline_path_line` | `startup_contracts::run_state::test_context_block_includes_usage_baseline_path_line` |
| `test_llm_to_bedrock_run_state` | `test_current_context_schema_includes_usage_baseline_sha256` | `startup_contracts::run_state::test_current_context_schema_includes_usage_baseline_sha256` |
| `test_llm_to_bedrock_run_state` | `test_mismatch_table_has_usage_baseline_sha256_row` | `startup_contracts::run_state::test_mismatch_table_has_usage_baseline_sha256_row` |
| `test_llm_to_bedrock_run_state` | `test_report_generator_reads_passed_baseline_path_not_bare_bedrock_run_dir` | `startup_contracts::run_state::test_report_generator_reads_passed_baseline_path_not_bare_bedrock_run_dir` |
| `test_llm_to_bedrock_run_state` | `test_step_1_5_defers_the_write_instead_of_writing_immediately` | `startup_contracts::run_state::test_step_1_5_defers_the_write_instead_of_writing_immediately` |
| `test_llm_to_bedrock_run_state` | `test_tiebreak_no_longer_claims_unconditional_lexicographic_chronology` | `startup_contracts::run_state::test_tiebreak_no_longer_claims_unconditional_lexicographic_chronology` |
| `test_llm_to_bedrock_run_state` | `test_tiebreak_surfaces_genuine_ties_to_the_user_instead_of_guessing` | `startup_contracts::run_state::test_tiebreak_surfaces_genuine_ties_to_the_user_instead_of_guessing` |
| `test_llm_to_bedrock_run_state` | `test_tiebreak_uses_last_updated_as_primary_sort_key` | `startup_contracts::run_state::test_tiebreak_uses_last_updated_as_primary_sort_key` |
| `test_llm_to_bedrock_run_state` | `test_usage_baseline_write_step_in_a3_comes_after_bedrock_run_dir_is_set` | `startup_contracts::run_state::test_usage_baseline_write_step_in_a3_comes_after_bedrock_run_dir_is_set` |
| `test_openrouter_byok_detection_fixes` | `test_azure_discover_reentry_checks_manifest_not_just_profile_existence` | `startup_contracts::openrouter_byok::test_azure_discover_reentry_checks_manifest_not_just_profile_existence` |
| `test_openrouter_byok_detection_fixes` | `test_azure_discover_reentry_still_reoffers_consent_for_fully_completed_run` | `startup_contracts::openrouter_byok::test_azure_discover_reentry_still_reoffers_consent_for_fully_completed_run` |
| `test_openrouter_byok_detection_fixes` | `test_azure_discover_routes_pre_analytics_manifest_to_unknown_provenance` | `startup_contracts::openrouter_byok::test_azure_discover_routes_pre_analytics_manifest_to_unknown_provenance` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_derive_utc_midnight_window_not_raw_now` | `startup_contracts::openrouter_byok::test_both_copies_derive_utc_midnight_window_not_raw_now` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_document_uncertain_classification_for_both_combinations` | `startup_contracts::openrouter_byok::test_both_copies_document_uncertain_classification_for_both_combinations` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_persist_workspace_scope_in_manifest_schema` | `startup_contracts::openrouter_byok::test_both_copies_persist_workspace_scope_in_manifest_schema` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_record_window_bounds_in_manifest_and_reuse_on_retry` | `startup_contracts::openrouter_byok::test_both_copies_record_window_bounds_in_manifest_and_reuse_on_retry` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_resume_reads_workspace_scope_before_reasking` | `startup_contracts::openrouter_byok::test_both_copies_resume_reads_workspace_scope_before_reasking` |
| `test_openrouter_byok_detection_fixes` | `test_both_copies_update_cost_provenance_enum_to_include_uncertain` | `startup_contracts::openrouter_byok::test_both_copies_update_cost_provenance_enum_to_include_uncertain` |
| `test_openrouter_byok_detection_fixes` | `test_both_openrouter_discover_copies_reuse_original_capture_window_on_retry` | `startup_contracts::openrouter_byok::test_both_openrouter_discover_copies_reuse_original_capture_window_on_retry` |
| `test_openrouter_byok_detection_fixes` | `test_byok_passthrough_nonzero_fee_fixture_through_real_baseline_consumer` | `startup_contracts::openrouter_byok::test_byok_passthrough_nonzero_fee_fixture_through_real_baseline_consumer` |
| `test_openrouter_byok_detection_fixes` | `test_cost_provenance_handles_multiple_permaslugs_per_short_model` | `startup_contracts::openrouter_byok::test_cost_provenance_handles_multiple_permaslugs_per_short_model` |
| `test_openrouter_byok_detection_fixes` | `test_cost_provenance_join_fixture_openai_gpt41_classifies_mixed` | `startup_contracts::openrouter_byok::test_cost_provenance_join_fixture_openai_gpt41_classifies_mixed` |
| `test_openrouter_byok_detection_fixes` | `test_cost_provenance_join_key_is_model_permaslug_not_model` | `startup_contracts::openrouter_byok::test_cost_provenance_join_key_is_model_permaslug_not_model` |
| `test_openrouter_byok_detection_fixes` | `test_delayed_retry_reuses_original_window_not_a_fresh_midnight` | `startup_contracts::openrouter_byok::test_delayed_retry_reuses_original_window_not_a_fresh_midnight` |
| `test_openrouter_byok_detection_fixes` | `test_gcp_discover_checks_analytics_entry_before_routing_activity_ok_to_step3` | `startup_contracts::openrouter_byok::test_gcp_discover_checks_analytics_entry_before_routing_activity_ok_to_step3` |
| `test_openrouter_byok_detection_fixes` | `test_gcp_discover_routes_pre_analytics_manifest_to_unknown_provenance` | `startup_contracts::openrouter_byok::test_gcp_discover_routes_pre_analytics_manifest_to_unknown_provenance` |
| `test_openrouter_byok_detection_fixes` | `test_no_blanket_get_only_claim_without_analytics_exemption_anywhere` | `startup_contracts::openrouter_byok::test_no_blanket_get_only_claim_without_analytics_exemption_anywhere` |
| `test_openrouter_byok_detection_fixes` | `test_noon_capture_boundary_fixture_matches_activity_api_30_completed_days` | `startup_contracts::openrouter_byok::test_noon_capture_boundary_fixture_matches_activity_api_30_completed_days` |
| `test_openrouter_byok_detection_fixes` | `test_openrouter_discover_copies_remain_mirrored_for_join_fix` | `startup_contracts::openrouter_byok::test_openrouter_discover_copies_remain_mirrored_for_join_fix` |
| `test_openrouter_byok_detection_fixes` | `test_openrouter_discover_copies_remain_mirrored_for_retry_window_fix` | `startup_contracts::openrouter_byok::test_openrouter_discover_copies_remain_mirrored_for_retry_window_fix` |
| `test_openrouter_byok_detection_fixes` | `test_openrouter_discover_copies_remain_mirrored_for_uncertain_fix` | `startup_contracts::openrouter_byok::test_openrouter_discover_copies_remain_mirrored_for_uncertain_fix` |
| `test_openrouter_byok_detection_fixes` | `test_openrouter_discover_copies_remain_mirrored_for_utc_window_fix` | `startup_contracts::openrouter_byok::test_openrouter_discover_copies_remain_mirrored_for_utc_window_fix` |
| `test_openrouter_byok_detection_fixes` | `test_rollup_byok_passthrough_plus_unknown_also_resolves_uncertain` | `startup_contracts::openrouter_byok::test_rollup_byok_passthrough_plus_unknown_also_resolves_uncertain` |
| `test_openrouter_byok_detection_fixes` | `test_rollup_uncertain_classification_does_not_double_count_or_drop_tokens` | `startup_contracts::openrouter_byok::test_rollup_uncertain_classification_does_not_double_count_or_drop_tokens` |
| `test_openrouter_byok_detection_fixes` | `test_scope_boundary_exempts_post_analytics_query` | `startup_contracts::openrouter_byok::test_scope_boundary_exempts_post_analytics_query` |
| `test_openrouter_byok_detection_fixes` | `test_skill_md_byok_passthrough_bullet_has_no_mixed_language` | `startup_contracts::openrouter_byok::test_skill_md_byok_passthrough_bullet_has_no_mixed_language` |
| `test_openrouter_byok_detection_fixes` | `test_skill_md_byok_passthrough_discloses_nonzero_router_fee` | `startup_contracts::openrouter_byok::test_skill_md_byok_passthrough_discloses_nonzero_router_fee` |
| `test_openrouter_byok_detection_fixes` | `test_skill_md_mixed_exclusion_carries_undercount_disclosure` | `startup_contracts::openrouter_byok::test_skill_md_mixed_exclusion_carries_undercount_disclosure` |
| `test_openrouter_byok_detection_fixes` | `test_skill_md_step15_handles_uncertain_cost_provenance` | `startup_contracts::openrouter_byok::test_skill_md_step15_handles_uncertain_cost_provenance` |
| `test_openrouter_byok_detection_fixes` | `test_step4_cleanup_gated_on_both_rows_terminal` | `startup_contracts::openrouter_byok::test_step4_cleanup_gated_on_both_rows_terminal` |
| `test_openrouter_byok_detection_fixes` | `test_step4_cleanup_pending_retry_does_not_proceed_with_merge` | `startup_contracts::openrouter_byok::test_step4_cleanup_pending_retry_does_not_proceed_with_merge` |
| `test_openrouter_byok_detection_fixes` | `test_three_entry_pre_analytics_manifest_routes_through_both_callers` | `startup_contracts::openrouter_byok::test_three_entry_pre_analytics_manifest_routes_through_both_callers` |
| `test_openrouter_byok_detection_fixes` | `test_uncertain_row_excluded_from_baseline_sum_not_double_counted` | `startup_contracts::openrouter_byok::test_uncertain_row_excluded_from_baseline_sum_not_double_counted` |
| `test_openrouter_byok_detection_fixes` | `test_workspace_a_interrupted_retry_fixture_does_not_pick_up_workspace_b` | `startup_contracts::openrouter_byok::test_workspace_a_interrupted_retry_fixture_does_not_pick_up_workspace_b` |
| `test_report_generator_capture_warnings_gate` | `test_empty_capture_warnings_still_allows_unqualified_percentage` | `startup_contracts::cost_compare::test_empty_capture_warnings_still_allows_unqualified_percentage` |
| `test_report_generator_capture_warnings_gate` | `test_full_window_available_cost_baseline_with_mixed_exclusion_is_qualified` | `startup_contracts::cost_compare::test_full_window_available_cost_baseline_with_mixed_exclusion_is_qualified` |
| `test_report_generator_capture_warnings_gate` | `test_section_6_3_reads_capture_warnings_independently` | `startup_contracts::cost_compare::test_section_6_3_reads_capture_warnings_independently` |
| `test_report_generator_cost_pricing` | `test_baseline_covered_pair_prices_both_sides_from_baseline_volume` | `startup_contracts::cost_compare::test_baseline_covered_pair_prices_both_sides_from_baseline_volume` |
| `test_report_generator_cost_pricing` | `test_gpt6_1_sol_ultrafast_applies_6x_multiplier` | `startup_contracts::cost_compare::test_gpt6_1_sol_ultrafast_applies_6x_multiplier` |
| `test_report_generator_cost_pricing` | `test_gpt6_sol_ultrafast_request_reports_cost_unavailable` | `startup_contracts::cost_compare::test_gpt6_sol_ultrafast_request_reports_cost_unavailable` |
| `test_report_generator_cost_pricing` | `test_no_baseline_at_all_matches_pre_fix_golden_dataset_behavior` | `startup_contracts::cost_compare::test_no_baseline_at_all_matches_pre_fix_golden_dataset_behavior` |
| `test_report_generator_cost_pricing` | `test_pair_not_covered_by_baseline_falls_back_to_golden_dataset_sample` | `startup_contracts::cost_compare::test_pair_not_covered_by_baseline_falls_back_to_golden_dataset_sample` |
| `test_report_generator_cost_pricing` | `test_pair_with_no_service_tier_entry_prices_as_standard_unchanged` | `startup_contracts::cost_compare::test_pair_with_no_service_tier_entry_prices_as_standard_unchanged` |
| `test_resume_and_sentinel_consistency` | `test_anthropic_sentinel_condition_has_no_sdk_method_qualifier` | `startup_contracts::discover::test_anthropic_sentinel_condition_has_no_sdk_method_qualifier` |
| `test_resume_and_sentinel_consistency` | `test_azure_discover_checks_anthropic_row_status_not_bare_existence` | `startup_contracts::discover::test_azure_discover_checks_anthropic_row_status_not_bare_existence` |
| `test_resume_and_sentinel_consistency` | `test_azure_openai_discover_now_appends_workload_row` | `startup_contracts::discover::test_azure_openai_discover_now_appends_workload_row` |
| `test_resume_and_sentinel_consistency` | `test_gcp_discover_checks_anthropic_row_status_not_bare_existence` | `startup_contracts::discover::test_gcp_discover_checks_anthropic_row_status_not_bare_existence` |
| `test_usage_api_request_params` | `test_anthropic_consent_block_discloses_preselection_probe` | `startup_contracts::usage_api::test_anthropic_consent_block_discloses_preselection_probe` |
| `test_usage_api_request_params` | `test_anthropic_cost_report_carries_workspace_id_per_row` | `startup_contracts::usage_api::test_anthropic_cost_report_carries_workspace_id_per_row` |
| `test_usage_api_request_params` | `test_anthropic_cost_report_uses_single_call_with_compound_group_by` | `startup_contracts::usage_api::test_anthropic_cost_report_uses_single_call_with_compound_group_by` |
| `test_usage_api_request_params` | `test_anthropic_probe_uses_bracket_group_by` | `startup_contracts::usage_api::test_anthropic_probe_uses_bracket_group_by` |
| `test_usage_api_request_params` | `test_anthropic_usage_messages_row_uses_bracket_group_by` | `startup_contracts::usage_api::test_anthropic_usage_messages_row_uses_bracket_group_by` |
| `test_usage_api_request_params` | `test_azure_openai_costs_row_keeps_limit_180` | `startup_contracts::usage_api::test_azure_openai_costs_row_keeps_limit_180` |
| `test_usage_api_request_params` | `test_azure_openai_has_copy_paste_trap_callout` | `startup_contracts::usage_api::test_azure_openai_has_copy_paste_trap_callout` |
| `test_usage_api_request_params` | `test_azure_openai_usage_rows_do_not_request_limit_180` | `startup_contracts::usage_api::test_azure_openai_usage_rows_do_not_request_limit_180` |
| `test_ai_capability_catalog_contracts` | `test_schema_has_no_unsupported_keywords` | `artifacts::tests::catalog::test_schema_has_no_unsupported_keywords` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_schema_is_valid_json_and_draft07_shaped` | `artifacts::tests::catalog::test_schema_is_valid_json_and_draft07_shaped` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_canonical_catalog_validates_against_schema` | `artifacts::tests::catalog::test_canonical_catalog_validates_against_schema` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_canonical_catalog_satisfies_semantic_invariants` | `artifacts::tests::catalog::test_canonical_catalog_satisfies_semantic_invariants` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_canonical_catalog_never_claims_an_implemented_adapter_or_route` | `artifacts::tests::catalog::test_canonical_catalog_never_claims_an_implemented_adapter_or_route` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_vendored_copy_is_byte_identical_to_canonical` | `artifacts::tests::catalog::test_vendored_copy_is_byte_identical_to_canonical` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_duplicate_provider_identity_is_rejected` | `artifacts::tests::catalog::test_duplicate_provider_identity_is_rejected` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_openrouter_as_a_direct_provider_identity_is_rejected` | `artifacts::tests::catalog::test_openrouter_as_a_direct_provider_identity_is_rejected` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_invalid_status_enum_value_is_rejected` | `artifacts::tests::catalog::test_invalid_status_enum_value_is_rejected` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_non_null_malformed_implemented_adapter_is_rejected` | `artifacts::tests::catalog::test_non_null_malformed_implemented_adapter_is_rejected` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_missing_required_field_is_rejected` | `artifacts::tests::catalog::test_missing_required_field_is_rejected` (in-crate) |
| `test_ai_capability_catalog_contracts` | `test_gateway_upstream_provider_absent_from_providers_is_rejected_semantically` | `artifacts::tests::catalog::test_gateway_upstream_provider_absent_from_providers_is_rejected_semantically` (in-crate) |


## One-to-one mapping (schema-driven AI contracts, #4806)

The six files the first slice split off. The Python files hold 115 test functions (the issue
text counted 107; the difference of 8 is two tests in each of four files, the schema self-validity pair); every one has a
Rust test of the same name. The in-crate tests (`artifacts::tests::<module>`) run through the
crate-private `SubsetValidator` (the Rust `validate_json_schema`), `build_contracts`,
`validate_shape` and the `startup artifacts` command; the `startup_contracts::rdfa` tests are
prose contracts (`check_rules` / `check`) and Rust models of the Python helper simulations.

| Python file | Python test | Rust test |
| --- | --- | --- |
| `test_ai_migration_matrix_contracts` | `test_combined_validation_references_both_tracks_and_all_categories` | `artifacts::tests::matrix::test_combined_validation_references_both_tracks_and_all_categories` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_duplicate_or_overlapping_ai_sources_are_rejected` | `artifacts::tests::matrix::test_duplicate_or_overlapping_ai_sources_are_rejected` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_duplicate_or_overlapping_gateway_upstreams_are_rejected` | `artifacts::tests::matrix::test_duplicate_or_overlapping_gateway_upstreams_are_rejected` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_each_case_shared_artifacts_validate_directly` | `artifacts::tests::matrix::test_each_case_shared_artifacts_validate_directly` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_every_snapshot_validates_against_scenario_schema` | `artifacts::tests::matrix::test_every_snapshot_validates_against_scenario_schema` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_future_structural_identity_does_not_claim_runtime_support` | `artifacts::tests::matrix::test_future_structural_identity_does_not_claim_runtime_support` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_gateway_upstreams_cannot_omit_source_model_details` | `artifacts::tests::matrix::test_gateway_upstreams_cannot_omit_source_model_details` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_gateway_upstreams_must_match_retained_ai_sources_exactly` | `artifacts::tests::matrix::test_gateway_upstreams_must_match_retained_ai_sources_exactly` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_heroku_composes_with_each_direct_ai_provider` | `artifacts::tests::matrix::test_heroku_composes_with_each_direct_ai_provider` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_integration_validation_accepts_uppercase_and_lowercase_run_ids` | `artifacts::tests::matrix::test_integration_validation_accepts_uppercase_and_lowercase_run_ids` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_manifest_has_fourteen_representative_scenarios_and_all_modes` | `artifacts::tests::matrix::test_manifest_has_fourteen_representative_scenarios_and_all_modes` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_multi_cardinality_openrouter_preserves_exact_associations` | `artifacts::tests::matrix::test_multi_cardinality_openrouter_preserves_exact_associations` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_openai_provider_preserves_direct_and_azure_endpoint_provenance` | `artifacts::tests::matrix::test_openai_provider_preserves_direct_and_azure_endpoint_provenance` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_openrouter_dimensions_compose_with_each_infrastructure_source` | `artifacts::tests::matrix::test_openrouter_dimensions_compose_with_each_infrastructure_source` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_overlapping_explicit_and_service_less_upstream_for_one_retained_source_is_rejected` | `artifacts::tests::matrix::test_overlapping_explicit_and_service_less_upstream_for_one_retained_source_is_rejected` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_required_artifacts_and_verdicts_match_each_mode` | `artifacts::tests::matrix::test_required_artifacts_and_verdicts_match_each_mode` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_schema_rejects_invalid_modes_ids_cardinality_and_sentinels` | `artifacts::tests::matrix::test_schema_rejects_invalid_modes_ids_cardinality_and_sentinels` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_success_verdict_rejects_any_failed_child` | `artifacts::tests::matrix::test_success_verdict_rejects_any_failed_child` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_success_verdict_rejects_empty_references` | `artifacts::tests::matrix::test_success_verdict_rejects_empty_references` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_supported_axes_have_independent_representatives` | `artifacts::tests::matrix::test_supported_axes_have_independent_representatives` (in-crate) |
| `test_ai_migration_matrix_contracts` | `test_unresolved_gateway_upstreams_are_explicit_and_unambiguous` | `artifacts::tests::matrix::test_unresolved_gateway_upstreams_are_explicit_and_unambiguous` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_dropped_model_under_a_resolved_upstream_is_rejected` | `artifacts::tests::workload::test_dropped_model_under_a_resolved_upstream_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_duplicate_ai_source_identity_is_rejected` | `artifacts::tests::workload::test_duplicate_ai_source_identity_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_empty_model_observations_on_an_observed_source_is_rejected` | `artifacts::tests::workload::test_empty_model_observations_on_an_observed_source_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_empty_upstreams_on_an_observed_gateway_is_rejected` | `artifacts::tests::workload::test_empty_upstreams_on_an_observed_gateway_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_fabricated_unknown_model_id_is_rejected` | `artifacts::tests::workload::test_fabricated_unknown_model_id_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_invented_upstream_on_an_unresolved_gateway_is_rejected` | `artifacts::tests::workload::test_invented_upstream_on_an_unresolved_gateway_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_openai_direct_and_azure_fixture_preserves_distinct_provenance` | `artifacts::tests::workload::test_openai_direct_and_azure_fixture_preserves_distinct_provenance` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_openrouter_as_a_direct_provider_is_rejected` | `artifacts::tests::workload::test_openrouter_as_a_direct_provider_is_rejected` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_openrouter_multi_upstream_fixture_has_two_models_in_one_family` | `artifacts::tests::workload::test_openrouter_multi_upstream_fixture_has_two_models_in_one_family` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_positive_fixtures_project_exactly_into_pr410_semantics` | `artifacts::tests::workload::test_positive_fixtures_project_exactly_into_pr410_semantics` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_positive_fixtures_satisfy_semantic_invariants` | `artifacts::tests::workload::test_positive_fixtures_satisfy_semantic_invariants` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_positive_fixtures_validate_against_profile_schema` | `artifacts::tests::workload::test_positive_fixtures_validate_against_profile_schema` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_schema_has_no_unsupported_keywords` | `artifacts::tests::workload::test_schema_has_no_unsupported_keywords` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_schema_is_valid_json_and_draft07_shaped` | `artifacts::tests::workload::test_schema_is_valid_json_and_draft07_shaped` (in-crate) |
| `test_ai_workload_profile_contracts` | `test_unresolved_gateway_fixture_has_empty_upstreams` | `artifacts::tests::workload::test_unresolved_gateway_fixture_has_empty_upstreams` (in-crate) |
| `test_aws_design_ai_contracts` | `test_azure_scalar_ai_source_fails_the_target_only_schema_with_no_skill` | `artifacts::tests::design_ai::test_azure_scalar_ai_source_fails_the_target_only_schema_with_no_skill` (in-crate) |
| `test_aws_design_ai_contracts` | `test_azure_scalar_ai_source_validates_under_the_restored_azure_shape_contract` | `artifacts::tests::design_ai::test_azure_scalar_ai_source_validates_under_the_restored_azure_shape_contract` (in-crate) |
| `test_aws_design_ai_contracts` | `test_design_block_naming_an_untraceable_model_is_rejected` | `artifacts::tests::design_ai::test_design_block_naming_an_untraceable_model_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_design_block_without_exactly_one_target_is_rejected` | `artifacts::tests::design_ai::test_design_block_without_exactly_one_target_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` | `artifacts::tests::design_ai::test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_duplicate_ai_source_identity_is_rejected` | `artifacts::tests::design_ai::test_duplicate_ai_source_identity_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_fabricated_unknown_model_in_metadata_is_rejected` | `artifacts::tests::design_ai::test_fabricated_unknown_model_in_metadata_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` | `artifacts::tests::design_ai::test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` (in-crate) |
| `test_aws_design_ai_contracts` | `test_gcp_scalar_ai_source_fails_the_target_only_schema_with_no_skill` | `artifacts::tests::design_ai::test_gcp_scalar_ai_source_fails_the_target_only_schema_with_no_skill` (in-crate) |
| `test_aws_design_ai_contracts` | `test_gcp_scalar_ai_source_validates_under_the_gcp_shape_contract` | `artifacts::tests::design_ai::test_gcp_scalar_ai_source_validates_under_the_gcp_shape_contract` (in-crate) |
| `test_aws_design_ai_contracts` | `test_metadata_rejects_the_retired_single_scalar_ai_source` | `artifacts::tests::design_ai::test_metadata_rejects_the_retired_single_scalar_ai_source` (in-crate) |
| `test_aws_design_ai_contracts` | `test_model_reachable_only_through_gateway_upstream_passes_traceability` | `artifacts::tests::design_ai::test_model_reachable_only_through_gateway_upstream_passes_traceability` (in-crate) |
| `test_aws_design_ai_contracts` | `test_model_removed_from_both_source_and_gateway_upstream_fails_traceability` | `artifacts::tests::design_ai::test_model_removed_from_both_source_and_gateway_upstream_fails_traceability` (in-crate) |
| `test_aws_design_ai_contracts` | `test_multi_provider_fixture_carries_two_simultaneous_direct_sources` | `artifacts::tests::design_ai::test_multi_provider_fixture_carries_two_simultaneous_direct_sources` (in-crate) |
| `test_aws_design_ai_contracts` | `test_observed_source_with_empty_model_observations_is_rejected_by_profile_semantics` | `artifacts::tests::design_ai::test_observed_source_with_empty_model_observations_is_rejected_by_profile_semantics` (in-crate) |
| `test_aws_design_ai_contracts` | `test_old_single_scalar_ai_source_shape_is_rejected` | `artifacts::tests::design_ai::test_old_single_scalar_ai_source_shape_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_old_single_scalar_ai_source_with_one_provider_is_also_rejected` | `artifacts::tests::design_ai::test_old_single_scalar_ai_source_with_one_provider_is_also_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_openrouter_as_a_direct_provider_is_rejected` | `artifacts::tests::design_ai::test_openrouter_as_a_direct_provider_is_rejected` (in-crate) |
| `test_aws_design_ai_contracts` | `test_positive_fixtures_have_traceable_design_block_models` | `artifacts::tests::design_ai::test_positive_fixtures_have_traceable_design_block_models` (in-crate) |
| `test_aws_design_ai_contracts` | `test_positive_fixtures_project_exactly_into_pr415_semantics` | `artifacts::tests::design_ai::test_positive_fixtures_project_exactly_into_pr415_semantics` (in-crate) |
| `test_aws_design_ai_contracts` | `test_positive_fixtures_validate_against_design_ai_schema` | `artifacts::tests::design_ai::test_positive_fixtures_validate_against_design_ai_schema` (in-crate) |
| `test_aws_design_ai_contracts` | `test_schema_has_no_unsupported_keywords` | `artifacts::tests::design_ai::test_schema_has_no_unsupported_keywords` (in-crate) |
| `test_aws_design_ai_contracts` | `test_schema_is_valid_json_and_draft07_shaped` | `artifacts::tests::design_ai::test_schema_is_valid_json_and_draft07_shaped` (in-crate) |
| `test_estimation_ai_contracts` | `test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` | `artifacts::tests::estimation_ai::test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_duplicate_cost_by_source_identity_is_rejected` | `artifacts::tests::estimation_ai::test_duplicate_cost_by_source_identity_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` | `artifacts::tests::estimation_ai::test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` (in-crate) |
| `test_estimation_ai_contracts` | `test_malformed_cost_by_source_entry_missing_models_is_rejected` | `artifacts::tests::estimation_ai::test_malformed_cost_by_source_entry_missing_models_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_multi_provider_fixture_carries_two_simultaneous_direct_sources` | `artifacts::tests::estimation_ai::test_multi_provider_fixture_carries_two_simultaneous_direct_sources` (in-crate) |
| `test_estimation_ai_contracts` | `test_openrouter_as_a_direct_cost_by_source_provider_is_rejected` | `artifacts::tests::estimation_ai::test_openrouter_as_a_direct_cost_by_source_provider_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_optimization_opportunity_missing_type_is_rejected` | `artifacts::tests::estimation_ai::test_optimization_opportunity_missing_type_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_overlapping_cost_by_source_identity_is_rejected` | `artifacts::tests::estimation_ai::test_overlapping_cost_by_source_identity_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_positive_fixtures_have_unique_cost_by_source_identity` | `artifacts::tests::estimation_ai::test_positive_fixtures_have_unique_cost_by_source_identity` (in-crate) |
| `test_estimation_ai_contracts` | `test_positive_fixtures_project_into_matching_design_source` | `artifacts::tests::estimation_ai::test_positive_fixtures_project_into_matching_design_source` (in-crate) |
| `test_estimation_ai_contracts` | `test_positive_fixtures_validate_against_estimation_ai_schema` | `artifacts::tests::estimation_ai::test_positive_fixtures_validate_against_estimation_ai_schema` (in-crate) |
| `test_estimation_ai_contracts` | `test_recommendation_requires_a_valid_path_enum` | `artifacts::tests::estimation_ai::test_recommendation_requires_a_valid_path_enum` (in-crate) |
| `test_estimation_ai_contracts` | `test_schema_has_no_unsupported_keywords` | `artifacts::tests::estimation_ai::test_schema_has_no_unsupported_keywords` (in-crate) |
| `test_estimation_ai_contracts` | `test_schema_is_valid_json_and_draft07_shaped` | `artifacts::tests::estimation_ai::test_schema_is_valid_json_and_draft07_shaped` (in-crate) |
| `test_estimation_ai_contracts` | `test_services_not_estimated_entry_requires_not_token_priced_reason` | `artifacts::tests::estimation_ai::test_services_not_estimated_entry_requires_not_token_priced_reason` (in-crate) |
| `test_estimation_ai_contracts` | `test_unavailable_target_pricing_preserves_unknown_costs_as_null` | `artifacts::tests::estimation_ai::test_unavailable_target_pricing_preserves_unknown_costs_as_null` (in-crate) |
| `test_estimation_ai_contracts` | `test_unknown_source_baseline_propagates_null_to_dependent_deltas` | `artifacts::tests::estimation_ai::test_unknown_source_baseline_propagates_null_to_dependent_deltas` (in-crate) |
| `test_estimation_ai_contracts` | `test_untraceable_model_under_resolved_design_source_is_rejected` | `artifacts::tests::estimation_ai::test_untraceable_model_under_resolved_design_source_is_rejected` (in-crate) |
| `test_estimation_ai_contracts` | `test_vendored_copies_are_byte_identical` | `artifacts::tests::estimation_ai::test_vendored_copies_are_byte_identical` (in-crate) |
| `test_generation_ai_contracts` | `test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` | `artifacts::tests::generation_ai::test_dropped_model_under_a_resolved_gateway_upstream_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_duplicate_artifacts_by_source_identity_is_rejected` | `artifacts::tests::generation_ai::test_duplicate_artifacts_by_source_identity_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` | `artifacts::tests::generation_ai::test_gateway_fixture_carries_multi_upstream_openrouter_fronting_two_providers` (in-crate) |
| `test_generation_ai_contracts` | `test_generated_files_entry_requires_at_least_one_file` | `artifacts::tests::generation_ai::test_generated_files_entry_requires_at_least_one_file` (in-crate) |
| `test_generation_ai_contracts` | `test_malformed_artifacts_by_source_entry_missing_models_is_rejected` | `artifacts::tests::generation_ai::test_malformed_artifacts_by_source_entry_missing_models_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_multi_provider_fixture_carries_two_simultaneous_direct_sources` | `artifacts::tests::generation_ai::test_multi_provider_fixture_carries_two_simultaneous_direct_sources` (in-crate) |
| `test_generation_ai_contracts` | `test_openrouter_as_a_direct_artifacts_by_source_provider_is_rejected` | `artifacts::tests::generation_ai::test_openrouter_as_a_direct_artifacts_by_source_provider_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_overlapping_artifacts_by_source_identity_is_rejected` | `artifacts::tests::generation_ai::test_overlapping_artifacts_by_source_identity_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_positive_fixtures_have_unique_artifacts_by_source_identity` | `artifacts::tests::generation_ai::test_positive_fixtures_have_unique_artifacts_by_source_identity` (in-crate) |
| `test_generation_ai_contracts` | `test_positive_fixtures_project_into_matching_estimation_source` | `artifacts::tests::generation_ai::test_positive_fixtures_project_into_matching_estimation_source` (in-crate) |
| `test_generation_ai_contracts` | `test_positive_fixtures_validate_against_generation_ai_schema` | `artifacts::tests::generation_ai::test_positive_fixtures_validate_against_generation_ai_schema` (in-crate) |
| `test_generation_ai_contracts` | `test_production_readiness_checklist_requires_at_least_five_items` | `artifacts::tests::generation_ai::test_production_readiness_checklist_requires_at_least_five_items` (in-crate) |
| `test_generation_ai_contracts` | `test_rollback_plan_mechanism_must_be_feature_flag` | `artifacts::tests::generation_ai::test_rollback_plan_mechanism_must_be_feature_flag` (in-crate) |
| `test_generation_ai_contracts` | `test_schema_has_no_unsupported_keywords` | `artifacts::tests::generation_ai::test_schema_has_no_unsupported_keywords` (in-crate) |
| `test_generation_ai_contracts` | `test_schema_is_valid_json_and_draft07_shaped` | `artifacts::tests::generation_ai::test_schema_is_valid_json_and_draft07_shaped` (in-crate) |
| `test_generation_ai_contracts` | `test_untraceable_model_under_resolved_estimation_source_is_rejected` | `artifacts::tests::generation_ai::test_untraceable_model_under_resolved_estimation_source_is_rejected` (in-crate) |
| `test_generation_ai_contracts` | `test_vendored_copies_are_byte_identical` | `artifacts::tests::generation_ai::test_vendored_copies_are_byte_identical` (in-crate) |
| `test_azure_rdfa_discover_contracts` | `test_absent_report_guidance_is_reachable_before_trigger` | `startup_contracts::rdfa::test_absent_report_guidance_is_reachable_before_trigger` |
| `test_azure_rdfa_discover_contracts` | `test_confidence_contract_requires_samples_and_known_window` | `startup_contracts::rdfa::test_confidence_contract_requires_samples_and_known_window` |
| `test_azure_rdfa_discover_contracts` | `test_corrupt_selected_report_records_preparation_warning` | `startup_contracts::rdfa::test_corrupt_selected_report_records_preparation_warning` |
| `test_azure_rdfa_discover_contracts` | `test_discover_and_assembler_include_rdfa_everywhere` | `startup_contracts::rdfa::test_discover_and_assembler_include_rdfa_everywhere` |
| `test_azure_rdfa_discover_contracts` | `test_error_empty_and_sampled_zero_have_distinct_eligibility` | `startup_contracts::rdfa::test_error_empty_and_sampled_zero_have_distinct_eligibility` |
| `test_azure_rdfa_discover_contracts` | `test_external_directory_reentry_seeds_recorded_source_without_new_input` | `startup_contracts::rdfa::test_external_directory_reentry_seeds_recorded_source_without_new_input` |
| `test_azure_rdfa_discover_contracts` | `test_final_rdfa_inventory_passes_real_artifact_validator` | `artifacts::tests::rdfa::test_final_rdfa_inventory_passes_real_artifact_validator (in-crate)` |
| `test_azure_rdfa_discover_contracts` | `test_fixed_and_autoscaled_aks_bounds_are_valid` | `startup_contracts::rdfa::test_fixed_and_autoscaled_aks_bounds_are_valid` |
| `test_azure_rdfa_discover_contracts` | `test_fixture_corrupt_zip_is_invalid_and_tie_break_is_deterministic` | `startup_contracts::rdfa::test_fixture_corrupt_zip_is_invalid_and_tie_break_is_deterministic` |
| `test_azure_rdfa_discover_contracts` | `test_live_and_rdfa_use_same_node_pool_key` | `startup_contracts::rdfa::test_live_and_rdfa_use_same_node_pool_key` |
| `test_azure_rdfa_discover_contracts` | `test_main_window_owns_reentry_and_preparation_failure_contracts` | `startup_contracts::rdfa::test_main_window_owns_reentry_and_preparation_failure_contracts` |
| `test_azure_rdfa_discover_contracts` | `test_metrics_batches_flatten_real_envelopes_and_accept_empty_batch` | `startup_contracts::rdfa::test_metrics_batches_flatten_real_envelopes_and_accept_empty_batch` |
| `test_azure_rdfa_discover_contracts` | `test_partial_nested_report_keeps_valid_inner_and_warns_for_bad_inner` | `startup_contracts::rdfa::test_partial_nested_report_keeps_valid_inner_and_warns_for_bad_inner` |
| `test_azure_rdfa_discover_contracts` | `test_rdfa_ml_workspace_contributes_and_mixed_identity_reconciles` | `startup_contracts::rdfa::test_rdfa_ml_workspace_contributes_and_mixed_identity_reconciles` |
| `test_azure_rdfa_discover_contracts` | `test_report_only_and_app_code_plus_report_are_admitted` | `startup_contracts::rdfa::test_report_only_and_app_code_plus_report_are_admitted` |
| `test_azure_rdfa_discover_contracts` | `test_rw_fragment_owns_no_executable_preparation` | `startup_contracts::rdfa::test_rw_fragment_owns_no_executable_preparation` |
| `test_azure_rdfa_discover_contracts` | `test_schema_registers_rdfa_inventory_and_ai_provenance` | `startup_contracts::rdfa::test_schema_registers_rdfa_inventory_and_ai_provenance` |
| `test_azure_rdfa_discover_contracts` | `test_source_identity_reuses_only_exact_source_and_replacement_wins` | `startup_contracts::rdfa::test_source_identity_reuses_only_exact_source_and_replacement_wins` |
| `test_azure_rdfa_discover_contracts` | `test_vm_os_disk_maps_to_canonical_nested_shape` | `startup_contracts::rdfa::test_vm_os_disk_maps_to_canonical_nested_shape` |
| `test_azure_rdfa_discover_contracts` | `test_zero_node_aks_pool_gets_eks_valid_max_size` | `startup_contracts::rdfa::test_zero_node_aks_pool_gets_eks_valid_max_size` |

`test_an_exact_duplicate_snapshot_source_is_refused_by_the_schema` (matrix) has no Python
counterpart: the Python `test_every_snapshot_validates_against_scenario_schema` has no negative
case, and the mutation for its semantic half (an overlapping, non-identical source) is refused by
the schema's `uniqueItems` when the source is a byte-identical duplicate, so both mutations are kept.

## Non-vacuity

`check_rules` / `check` execute the mutation proof inside every test. For example, in
`test_step_3a_does_not_emit_its_own_close` the edited copy of `estimate-assemble.md` gains the
sentence `Your decision is saved at` inside Step 3a and the test requires the contract to fail
with "emits its own close". The `catalog.rs` positive checks carry their own edited-copy
assertions (a schema with `contains`, a duplicate provider, a claimed adapter, an edited vendored
copy), and the Python file's negative cases are kept as they were.

## Cases that executed Python

`test_gpt6_source_breakpoint.py` (7) and `test_report_generator_cost_pricing.py` (6) extracted the one
```python block of `agents/llm2bedrock-report-generator.md` (the `cost_compare.py` the agent writes
and runs) and ran it with a Python interpreter. Running Python is not allowed here and the block is
shipped agent content. `cost_compare.rs` therefore keeps the contract in two linked halves:

1. `PINNED` lists the 24 statements of the embedded script that carry the pricing decisions
   (breakpoint comparison `> 272_000`, the long/short cost formulas, the basis labels, the
   ultrafast rule and the output formats). A test requires each to be present in the committed
   block and fails (with the statement named) when one is edited.
2. `price` is an executable model of exactly those statements and the numeric cases of the 13 Python
   tests run on it and assert the same stdout lines (`$135.0000`, `$148.5000`, `+61.3%`, ...).

Limit, recorded: the model does not execute the committed script. A change to the script that
keeps every pinned statement can go unnoticed. The remaining compatibility question (whether the
embedded cost script becomes a `toolkit` command so the numeric tests run on the shipped logic) is
for the manager under the "remote managed-service Python payloads" rule of AGENTS.md.

## Intentional differences from the Python tests

* Tests that were tautologies over a function defined inside the Python test (the
  `capture_warnings` gate, the OpenRouter classification, rollup, window and routing models, the
  mixed-tier arithmetic) are ported as local Rust functions with the same fixtures. Like the Python
  they document the intended rule and do not read the plugin; the prose halves are real contracts.
* `Scope::Between` keeps Python's two independent `index` calls; `Scope::Span`/`Within` keep
  `index(end, start)`. A window anchored in a bounded section (`rung2[identity:identity + 2500]`) is
  measured from the anchor and is not clipped at the section end.
* Whitespace-normalised rules collapse the whole file first, so a window measured after a
  normalisation covers more of the original text than the Python slice (`Step 4` clean-up window).
* A case-insensitive rule uses lower-case needles; `A or B.lower()` with `A.lower() == B` is one
  case-insensitive needle.
* `test_estimate_md_step_4_is_the_only_close_owner` and the other multi-file checks assert each
  file, as the Python did; a missing file fails with the file name.
* `test_dd38bc2_spellings_and_openrouter_priority_preserved` asserts that "OpenRouter subroute" precedes
  `migration_path` inside a slice that starts at that anchor, which is true by construction in the
  Python as well; the port keeps the assertion and the harness proves the anchor itself is required.
* The Python `tmp_path` goldens for the Azure clarify fast path are not copied; #4408's tests run
  the asserters on the committed trees and on edited copies.
* Two Python assertions are implied by a stronger one in the same region and are not ported
  separately, because mutating the weaker needle always trips the stronger rule first:
  `"unavailable" in section.lower()` (implied by `"cost_unavailable" in section`, report-generator
  §6.3) and the bare `"model_permaslug" in section` (implied by the longer
  `join key is /activity's model_permaslug field` assertion). One design-mapping assertion
  (`"resource_id"` inside the 1200-character `**Required fields**` window) is a substring of a
  sibling assertion; its mutation must fail the rules but not for a specific reason (`Rule::loose`).

### #4806 notes

* **Non-vacuity of the schema-driven ports.** Each positive check runs on the committed fixture and
  on an edited copy: in-memory for a single document (`support::assert_rejected` also requires the
  unedited document to be accepted, so a negative mutation is always tied to an accepted control) and
  in a scratch copy of the fixture tree (`support::Tree`) for the matrix, whose contracts read the
  manifest, snapshots, state files and `integration-validation.json` from disk. Contracts that the
  Python wrote as helper `assert`s (`_assert_scenario_semantics`, `_assert_profile_semantics`, the
  model-traceability and projection helpers) are `Result`-returning functions so that both the
  positive and the negative direction are asserted.
* **Python `if not findings: pytest.raises ... else: assert findings`** (three matrix tests, the
  workload and the design duplicate-source tests) is kept as "the schema findings are non-empty or
  the semantic check fails" (`refused`); the duplicate/overlap cases additionally assert which half
  refuses where it is deterministic.
* **`sync-vendored.py --check`** (estimation and generation) is `toolkit_core::sync_vendored(plugin,
  check = true)`, the library behind `toolkit sync vendored --check`; the non-vacuity case drifts one
  vendored file in a scratch copy of `skills/`.
* **`build_contracts` / `load_manifest` / `validate_shape`** (design, Azure and GCP) run the
  crate's own `build_contracts` and `validate_shape` on the committed manifest and shape documents.
* **`subprocess` runs of `validate-artifacts.py`** (matrix integration-validation, RDfA final
  inventory) call `toolkit startup artifacts` in process with `--no-baseline --json`.
  The Python passed `--manifest` explicitly; the Rust command reads the same manifest from
  `--plugin-root`.
* **RDfA simulations.** The Python helpers defined inside the test file (`_eligible`,
  `_normalize_pool`, `_map_vm`, the fingerprint, the candidate discovery, the ZIP preparation and the
  reconcile model) are Rust functions with the same fixtures, like the other model tests above. No
  zip crate is in the workspace, so the tests carry a stored-entry zip writer and reader and an
  `is_zipfile` that looks for the end-of-central-directory record. Deflated entries and the unsafe
  member name sanitising of `extractall` are not modelled; no test depends on either.
* No Python assertion was dropped. Assertions are only strengthened: `_cases()` and the manifest
  loader return errors instead of asserting, set equalities are exact, the bad-inner-ZIP test also
  asserts exactly one warning.
