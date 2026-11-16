//! Viva #191 native execution seam.
//!
//! This handler is reachable only after the embedding admission has validated
//! a host-issued owner frontend capability on the actual socket peer. The
//! composition root performs the canonical task/member/grant checks before
//! this handler creates a workspace, and confirms the real OS child after.
use crate::api::schema::{
    ResponseResult, VivaExecutionStartParams, VivaExecutionStopParams, VivaPauseParams,
};
use crate::app::App;
use crate::embedding::ExecutionRequest;

use super::responses::{encode_error, encode_success};

impl App {
    // Viva modification (#191 P1): the domain registry owns composition facts;
    // Herdr owns only the live terminal views. Never execute .code-workspace tasks.
    pub(super) fn handle_viva_workspace(
        &mut self,
        id: String,
        params: crate::api::schema::VivaWorkspaceParams,
    ) -> String {
        let workspace = match crate::embedding::workspace(
            &params.action,
            params.value.as_deref(),
            params.project_id.as_deref(),
        ) {
            Ok(value) => value,
            Err(err) => return encode_error(id, "workspace_refused", err.to_string()),
        };
        if matches!(params.action.as_str(), "open" | "select") {
            let mut first = None;
            for folder in workspace["folders"].as_array().into_iter().flatten() {
                let Some(path) = folder["path"].as_str().map(std::path::PathBuf::from) else {
                    continue;
                };
                if !path.is_dir() {
                    continue;
                }
                let index = if let Some(index) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|ws| ws.identity_cwd == path)
                {
                    index
                } else {
                    match self.create_workspace_with_launch_env(path, false, Default::default()) {
                        Ok(index) => index,
                        Err(err) => {
                            return encode_error(id, "workspace_view_failed", err.to_string())
                        }
                    }
                };
                first.get_or_insert(index);
            }
            if let Some(index) = first {
                self.state.switch_workspace(index);
            }
        }
        encode_success(id, ResponseResult::VivaWorkspace { workspace })
    }

    // Viva modification: serialized with dispatch by the existing App queue.
    // A raw controller stop cannot bypass the same canonical stop seam as CLI.
    pub(super) fn handle_viva_host_stop(&mut self, id: String) -> String {
        let snapshot = match crate::embedding::snapshot() {
            Ok(snapshot) => snapshot,
            Err(err) => return encode_error(id, "shutdown_refused", err.to_string()),
        };
        for binding in snapshot["terminals"].as_array().into_iter().flatten() {
            if !binding["pane_id"].is_string() {
                continue;
            }
            let Some(execution_id) = binding["execution_id"].as_str() else {
                continue;
            };
            let response = self.handle_viva_execution_stop(
                id.clone(),
                VivaExecutionStopParams {
                    execution_id: execution_id.to_owned(),
                },
            );
            let failed = serde_json::from_str::<serde_json::Value>(&response)
                .map_or(true, |value| value.get("error").is_some());
            if failed {
                return response;
            }
        }
        self.state.should_quit = true;
        encode_success(id, ResponseResult::Ok {})
    }

    fn close_execution_workspace(&mut self, ws_idx: usize) {
        let terminal_ids = self.state.terminal_ids_for_workspace(ws_idx);
        self.state.close_workspaces(vec![ws_idx]);
        for terminal_id in terminal_ids {
            if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
                runtime.shutdown();
            }
        }
    }

    pub(super) fn handle_viva_execution_stop(
        &mut self,
        id: String,
        params: VivaExecutionStopParams,
    ) -> String {
        let plan = match crate::embedding::prepare_execution_stop(&params.execution_id) {
            Ok(plan) => plan,
            Err(err) => return encode_error(id, "execution_stop_refused", err.to_string()),
        };
        let Some(workspace_id) = plan.workspace_id.clone() else {
            return encode_error(
                id,
                "execution_not_started",
                "the execution has no live workspace",
            );
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            // The pane is already gone: record the process fact directly so a
            // stale binding cannot leave the execution running forever.
            if let Some(pane_id) = plan.pane_id.as_deref() {
                let _ = crate::embedding::execution_pane_exited(&workspace_id, pane_id, None);
            }
            return encode_success(
                id,
                ResponseResult::VivaExecutionStopped {
                    execution_id: plan.execution_id,
                    workspace_id: Some(workspace_id),
                    pane_id: plan.pane_id,
                },
            );
        };
        self.close_execution_workspace(ws_idx);
        encode_success(
            id,
            ResponseResult::VivaExecutionStopped {
                execution_id: plan.execution_id,
                workspace_id: Some(workspace_id),
                pane_id: plan.pane_id,
            },
        )
    }

    pub(super) fn handle_viva_host_pause(&mut self, id: String, params: VivaPauseParams) -> String {
        match crate::embedding::set_paused(params.paused) {
            Ok(()) => encode_success(
                id,
                ResponseResult::VivaHostPaused {
                    paused: params.paused,
                },
            ),
            Err(err) => encode_error(id, "pause_failed", err.to_string()),
        }
    }

    pub(super) fn handle_viva_execution_start(
        &mut self,
        id: String,
        params: VivaExecutionStartParams,
    ) -> String {
        let plan = match crate::embedding::prepare_execution(ExecutionRequest {
            task_id: params.task_id,
            member_id: params.member_id,
            grant_id: params.grant_id,
            request_key: params.request_key,
            argv: params.argv,
            cwd: std::path::PathBuf::from(params.cwd),
            worktree_id: params.worktree_id,
        }) {
            Ok(plan) => plan,
            Err(err) => return encode_error(id, "execution_refused", err.to_string()),
        };
        if plan.replayed {
            return encode_success(
                id,
                ResponseResult::VivaExecutionStarted {
                    execution_id: plan.execution_id,
                    replayed: true,
                    intent_state: plan.intent_state,
                    workspace_id: None,
                    pane_id: None,
                    pid: plan.existing_pid,
                },
            );
        }

        let ws_idx = match self.create_workspace_with_command(
            plan.cwd.clone(),
            true,
            plan.env.clone(),
            &plan.argv,
        ) {
            Ok(idx) => idx,
            Err(err) => {
                let _ = crate::embedding::execution_failed(&plan.execution_id);
                return encode_error(id, "execution_spawn_failed", err.to_string());
            }
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let root_pane = self.state.workspaces[ws_idx].tabs[0].root_pane;
        let public_pane_id = self.public_pane_id(ws_idx, root_pane);
        let terminal_id = self.state.workspaces[ws_idx]
            .terminal_id(root_pane)
            .map(ToString::to_string);
        let pid = self.state.workspaces[ws_idx]
            .terminal_id(root_pane)
            .and_then(|terminal_id| self.terminal_runtimes.get(terminal_id))
            .and_then(|runtime| runtime.child_pid());

        let (Some(public_pane_id), Some(terminal_id), Some(pid)) =
            (public_pane_id, terminal_id, pid)
        else {
            // Never leave an untracked child beside a canonical execution.
            self.close_execution_workspace(ws_idx);
            let _ = crate::embedding::execution_failed(&plan.execution_id);
            return encode_error(
                id,
                "execution_spawn_failed",
                "the native pane did not expose a real child process",
            );
        };

        if let Err(err) = crate::embedding::execution_started(
            &plan.execution_id,
            &workspace_id,
            &public_pane_id,
            &terminal_id,
            pid,
        ) {
            self.close_execution_workspace(ws_idx);
            let _ = crate::embedding::execution_failed(&plan.execution_id);
            return encode_error(id, "execution_confirm_failed", err.to_string());
        }
        if !plan.label.trim().is_empty() {
            if let Some(workspace) = self.state.workspaces.get_mut(ws_idx) {
                workspace.set_custom_name(plan.label);
            }
        }
        encode_success(
            id,
            ResponseResult::VivaExecutionStarted {
                execution_id: plan.execution_id,
                replayed: false,
                intent_state: "confirmed".into(),
                workspace_id: Some(workspace_id),
                pane_id: Some(public_pane_id),
                pid: Some(pid),
            },
        )
    }
}
