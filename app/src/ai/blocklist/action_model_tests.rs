use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::AIAgentActionResultType;
use serde_json::json;

fn make_action_result(id: &str) -> Arc<AIAgentActionResult> {
    Arc::new(AIAgentActionResult {
        id: AIAgentActionId::from(id.to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result: AIAgentActionResultType::InitProject,
    })
}

fn count_startable_actions_for_pass(phases: &[(RunningActionPhase, bool)]) -> usize {
    let mut current_phase = None;
    let mut count = 0;

    for (phase, can_autoexecute) in phases {
        if let Some(current_phase) = current_phase {
            if !can_start_action_with_current_phase(current_phase, *phase, *can_autoexecute) {
                break;
            }
        }

        count += 1;
        current_phase = Some(*phase);

        if matches!(*phase, RunningActionPhase::Serial) {
            break;
        }
    }

    count
}

fn call_mcp_tool_action() -> AIAgentActionType {
    AIAgentActionType::CallMCPTool {
        server_id: None,
        name: "list_tools".to_string(),
        input: json!({}),
    }
}

#[test]
fn call_mcp_tool_action_phase_is_parallel_readonly() {
    let phase = execute::BlocklistAIActionExecutor::static_parallel_phase_for_action(
        &call_mcp_tool_action(),
    );
    assert_eq!(
        phase,
        Some(RunningActionPhase::Parallel(
            execute::ParallelExecutionPolicy::ReadOnlyLocalContext
        ))
    );
}

#[test]
fn multiple_call_mcp_tool_actions_run_in_same_parallel_batch() {
    let phase = execute::BlocklistAIActionExecutor::static_parallel_phase_for_action(
        &call_mcp_tool_action(),
    )
    .expect("callMCPTool should be read-only parallel");
    let actions = vec![(phase, true), (phase, true), (phase, true)];
    let actions_with_confirmation_block = vec![(phase, true), (phase, false), (phase, true)];

    assert_eq!(count_startable_actions_for_pass(&actions), 3);
    assert_eq!(
        count_startable_actions_for_pass(&actions_with_confirmation_block),
        1
    );
}

#[test]
fn parallel_phase_only_admits_matching_autoexecutable_actions() {
    let phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);

    assert!(can_start_action_with_current_phase(phase, phase, true));
    assert!(!can_start_action_with_current_phase(phase, phase, false));
    assert!(!can_start_action_with_current_phase(
        phase,
        RunningActionPhase::Serial,
        true
    ));
    assert!(!can_start_action_with_current_phase(
        RunningActionPhase::Serial,
        phase,
        true
    ));
}

#[test]
fn phased_scheduling_stops_at_serial_barrier_and_resumes_afterward() {
    let read_only_phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);
    let actions = vec![
        (read_only_phase, true),
        (read_only_phase, true),
        (RunningActionPhase::Serial, true),
        (read_only_phase, true),
        (read_only_phase, true),
    ];

    assert_eq!(count_startable_actions_for_pass(&actions), 2);
    assert_eq!(count_startable_actions_for_pass(&actions[2..]), 1);
    assert_eq!(count_startable_actions_for_pass(&actions[3..]), 2);
}

#[test]
fn finished_results_stay_in_original_action_order() {
    let action_order = HashMap::from([
        (AIAgentActionId::from("first".to_owned()), 0),
        (AIAgentActionId::from("second".to_owned()), 1),
        (AIAgentActionId::from("third".to_owned()), 2),
    ]);
    let mut finished_results = [
        make_action_result("third"),
        make_action_result("first"),
        make_action_result("second"),
    ];

    finished_results
        .sort_by_key(|result| action_order.get(&result.id).copied().unwrap_or(usize::MAX));

    assert_eq!(
        finished_results[0].id,
        AIAgentActionId::from("first".to_owned())
    );
    assert_eq!(
        finished_results[1].id,
        AIAgentActionId::from("second".to_owned())
    );
    assert_eq!(
        finished_results[2].id,
        AIAgentActionId::from("third".to_owned())
    );
}
