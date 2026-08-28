#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::config::{Condition, PluginDeclaration, RuntimeDeclaration};
use crate::config_mode::{ResolutionIntent, ResolutionState};
use crate::model::{
    PluginRuntime, PluginSpec, RuntimeConfiguration as PluginRuntimeConfiguration, SetupOperation,
};

const CONDITION_TIMEOUT: Duration = Duration::from_secs(5);
const CONDITION_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessOutcome {
    Exited(i32),
    Signaled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessFailure {
    Spawn(String),
    Monitor(String),
}

trait ConditionProcess {
    fn run(
        &self,
        predicate: &str,
        working_dir: &Path,
        timeout: Duration,
    ) -> std::result::Result<ProcessOutcome, ProcessFailure>;
}

struct ShellConditionRunner;

impl ConditionProcess for ShellConditionRunner {
    fn run(
        &self,
        predicate: &str,
        working_dir: &Path,
        timeout: Duration,
    ) -> std::result::Result<ProcessOutcome, ProcessFailure> {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", predicate])
            .current_dir(working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        let mut child =
            command.spawn().map_err(|error| ProcessFailure::Spawn(error.to_string()))?;
        let started = Instant::now();

        loop {
            let status =
                child.try_wait().map_err(|error| ProcessFailure::Monitor(error.to_string()))?;
            if let Some(status) = status {
                return Ok(outcome_from_status(status));
            }
            if started.elapsed() >= timeout {
                if let Some(status) =
                    child.try_wait().map_err(|error| ProcessFailure::Monitor(error.to_string()))?
                {
                    return Ok(outcome_from_status(status));
                }
                terminate_condition_process(&mut child)?;
                return Ok(ProcessOutcome::TimedOut);
            }
            std::thread::sleep(CONDITION_POLL_INTERVAL);
        }
    }
}

#[cfg(unix)]
fn terminate_condition_process(child: &mut Child) -> std::result::Result<(), ProcessFailure> {
    let process_group =
        i32::try_from(child.id()).map_err(|error| ProcessFailure::Monitor(error.to_string()))?;
    // SAFETY: `process_group` is the positive PID returned for the child that this module spawned
    // as a process-group leader. Negating it targets that group and does not dereference memory.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(ProcessFailure::Monitor(error.to_string()));
        }
    }
    child.wait().map_err(|error| ProcessFailure::Monitor(error.to_string()))?;
    Ok(())
}

#[cfg(not(unix))]
fn terminate_condition_process(child: &mut Child) -> std::result::Result<(), ProcessFailure> {
    child.kill().map_err(|error| ProcessFailure::Monitor(error.to_string()))?;
    child.wait().map_err(|error| ProcessFailure::Monitor(error.to_string()))?;
    Ok(())
}

fn outcome_from_status(status: std::process::ExitStatus) -> ProcessOutcome {
    match status.code() {
        Some(code) => ProcessOutcome::Exited(code),
        None => ProcessOutcome::Signaled,
    }
}

#[derive(Debug)]
pub(super) struct ResolvedPlugins {
    pub(super) plugins: Vec<PluginSpec>,
    pub(super) state: ResolutionState,
}

pub(super) fn resolve_plugins(
    declarations: Vec<PluginDeclaration>,
    working_dir: &Path,
    intent: ResolutionIntent,
) -> Result<ResolvedPlugins> {
    resolve_plugins_with_runner(declarations, working_dir, intent, &ShellConditionRunner)
}

fn resolve_plugins_with_runner(
    declarations: Vec<PluginDeclaration>,
    working_dir: &Path,
    intent: ResolutionIntent,
    process: &impl ConditionProcess,
) -> Result<ResolvedPlugins> {
    let mut enabled = Vec::with_capacity(declarations.len());
    for declaration in &declarations {
        enabled.push(evaluate_condition(
            &declaration.enabled,
            declaration,
            "enabled",
            working_dir,
            process,
        )?);
    }

    let enabled_declarations: Vec<_> = declarations
        .into_iter()
        .zip(enabled)
        .filter_map(|(declaration, enabled)| enabled.then_some(declaration))
        .collect();

    let load_eligibility = if matches!(
        intent,
        ResolutionIntent::LoadEligibility | ResolutionIntent::RuntimeConfiguration
    ) {
        let mut eligibility = Vec::with_capacity(enabled_declarations.len());
        for declaration in &enabled_declarations {
            eligibility.push(evaluate_condition(
                &declaration.load_condition,
                declaration,
                "cond",
                working_dir,
                process,
            )?);
        }
        Some(eligibility)
    } else {
        None
    };
    let mut plugins = Vec::with_capacity(enabled_declarations.len());
    for (index, mut declaration) in enabled_declarations.into_iter().enumerate() {
        if matches!(intent, ResolutionIntent::RuntimeConfiguration)
            && load_eligibility.as_ref().is_some_and(|values| values[index])
        {
            let projection = project_runtime_configuration(&declaration, working_dir, process)?;
            declaration.spec.runtime = PluginRuntimeConfiguration::Selected(projection);
        } else {
            declaration.spec.runtime = PluginRuntimeConfiguration::Unresolved;
        }
        plugins.push(declaration.spec);
    }

    let state = match intent {
        ResolutionIntent::ManagedState => ResolutionState::ManagedState,
        ResolutionIntent::LoadEligibility => {
            ResolutionState::LoadEligibility(load_eligibility.expect("resolved above"))
        }
        ResolutionIntent::RuntimeConfiguration => {
            ResolutionState::RuntimeConfiguration(load_eligibility.expect("resolved above"))
        }
    };
    Ok(ResolvedPlugins { plugins, state })
}

fn project_runtime_configuration(
    declaration: &PluginDeclaration,
    working_dir: &Path,
    process: &impl ConditionProcess,
) -> Result<PluginRuntime> {
    let mut projection = PluginRuntime::default();
    project_runtime_declarations(
        &declaration.runtime,
        declaration,
        working_dir,
        process,
        &mut projection,
    )?;
    Ok(projection)
}

fn project_runtime_declarations(
    declarations: &[RuntimeDeclaration],
    plugin: &PluginDeclaration,
    working_dir: &Path,
    process: &impl ConditionProcess,
    projection: &mut PluginRuntime,
) -> Result<()> {
    for declaration in declarations {
        match declaration {
            RuntimeDeclaration::Option { key, value } => {
                projection
                    .setup
                    .push(SetupOperation::Option { key: key.clone(), value: value.clone() });
            }
            RuntimeDeclaration::Environment(operation) => {
                projection.setup.push(SetupOperation::Environment(operation.clone()));
            }
            RuntimeDeclaration::Binding(binding) => {
                projection.bindings.push(binding.clone());
            }
            RuntimeDeclaration::Branch { condition, then_declarations, else_declarations } => {
                let selected = evaluate_condition(condition, plugin, "if", working_dir, process)?;
                let selected = if selected { then_declarations } else { else_declarations };
                project_runtime_declarations(selected, plugin, working_dir, process, projection)?;
            }
        }
    }
    Ok(())
}

fn evaluate_condition(
    condition: &Condition,
    declaration: &PluginDeclaration,
    key: &str,
    working_dir: &Path,
    process: &impl ConditionProcess,
) -> Result<bool> {
    let Condition::Shell(predicate) = condition else {
        return Ok(matches!(condition, Condition::Bool(true)));
    };
    let plugin = declaration.spec.remote_id().unwrap_or(&declaration.spec.name);
    match process.run(predicate, working_dir, CONDITION_TIMEOUT) {
        Ok(ProcessOutcome::Exited(0)) => Ok(true),
        Ok(ProcessOutcome::Exited(_)) => Ok(false),
        Ok(ProcessOutcome::Signaled) => {
            bail!("plugin \"{plugin}\": {key} shell predicate terminated by a signal")
        }
        Ok(ProcessOutcome::TimedOut) => {
            bail!("plugin \"{plugin}\": {key} shell predicate timed out after 5 seconds")
        }
        Err(ProcessFailure::Spawn(error)) => {
            bail!("plugin \"{plugin}\": failed to start /bin/sh for {key} predicate: {error}")
        }
        Err(ProcessFailure::Monitor(error)) => {
            bail!("plugin \"{plugin}\": failed while running {key} predicate: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    struct FakeProcess {
        outcomes: RefCell<VecDeque<std::result::Result<ProcessOutcome, ProcessFailure>>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeProcess {
        fn new(
            outcomes: impl IntoIterator<Item = std::result::Result<ProcessOutcome, ProcessFailure>>,
        ) -> Self {
            Self {
                outcomes: RefCell::new(outcomes.into_iter().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ConditionProcess for FakeProcess {
        fn run(
            &self,
            predicate: &str,
            _working_dir: &Path,
            _timeout: Duration,
        ) -> std::result::Result<ProcessOutcome, ProcessFailure> {
            self.calls.borrow_mut().push(predicate.to_string());
            self.outcomes.borrow_mut().pop_front().expect("unexpected condition execution")
        }
    }

    #[test]
    fn projects_shell_conditions_once_in_declaration_order() {
        let parsed = crate::config::parse_config_document(
            r#"
plugin "user/first" enabled="first"
plugin "user/second" enabled=#true
plugin "user/third" enabled="third"
"#,
        )
        .unwrap();
        let process =
            FakeProcess::new([Ok(ProcessOutcome::Exited(0)), Ok(ProcessOutcome::Exited(9))]);

        let plugins = resolve_plugins_with_runner(
            parsed.plugins,
            Path::new("/config"),
            ResolutionIntent::ManagedState,
            &process,
        )
        .unwrap()
        .plugins;

        let names: Vec<_> = plugins.iter().map(|plugin| plugin.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);
        assert_eq!(process.calls.into_inner(), ["first", "third"]);
    }

    #[test]
    fn process_failures_are_hard_errors() {
        let cases = [
            (Ok(ProcessOutcome::Signaled), "terminated by a signal"),
            (Ok(ProcessOutcome::TimedOut), "timed out after 5 seconds"),
            (Err(ProcessFailure::Spawn("missing shell".into())), "failed to start /bin/sh"),
            (Err(ProcessFailure::Monitor("wait failed".into())), "failed while running"),
        ];

        for (outcome, expected) in cases {
            let parsed =
                crate::config::parse_config_document(r#"plugin "user/repo" enabled="predicate""#)
                    .unwrap();
            let process = FakeProcess::new([outcome]);

            let error = resolve_plugins_with_runner(
                parsed.plugins,
                Path::new("/config"),
                ResolutionIntent::ManagedState,
                &process,
            )
            .unwrap_err();

            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn resolves_enable_phase_before_load_phase_and_short_circuits_disabled_plugins() {
        let parsed = crate::config::parse_config_document(
            r#"
plugin "user/first" enabled="enable-first" cond="load-first"
plugin "user/disabled" enabled="enable-disabled" cond="must-not-run"
plugin "user/third" enabled="enable-third" cond="load-third"
"#,
        )
        .unwrap();
        let process = FakeProcess::new([
            Ok(ProcessOutcome::Exited(0)),
            Ok(ProcessOutcome::Exited(1)),
            Ok(ProcessOutcome::Exited(0)),
            Ok(ProcessOutcome::Exited(0)),
            Ok(ProcessOutcome::Exited(9)),
        ]);

        let resolved = resolve_plugins_with_runner(
            parsed.plugins,
            Path::new("/config"),
            ResolutionIntent::LoadEligibility,
            &process,
        )
        .unwrap();

        let names: Vec<_> = resolved.plugins.iter().map(|plugin| plugin.name.as_str()).collect();
        assert_eq!(names, ["first", "third"]);
        let ResolutionState::LoadEligibility(load_eligibility) = resolved.state else {
            panic!("expected Load Eligibility resolution state");
        };
        assert_eq!(load_eligibility, [true, false]);
        assert_eq!(
            process.calls.into_inner(),
            ["enable-first", "enable-disabled", "enable-third", "load-first", "load-third"]
        );
    }

    #[test]
    fn load_condition_process_failures_are_hard_errors() {
        let cases = [
            (Ok(ProcessOutcome::Signaled), "cond shell predicate terminated by a signal"),
            (Ok(ProcessOutcome::TimedOut), "cond shell predicate timed out after 5 seconds"),
            (Err(ProcessFailure::Spawn("missing shell".into())), "for cond predicate"),
            (Err(ProcessFailure::Monitor("wait failed".into())), "running cond predicate"),
        ];

        for (outcome, expected) in cases {
            let parsed =
                crate::config::parse_config_document(r#"plugin "user/repo" cond="predicate""#)
                    .unwrap();
            let process = FakeProcess::new([outcome]);

            let error = resolve_plugins_with_runner(
                parsed.plugins,
                Path::new("/config"),
                ResolutionIntent::LoadEligibility,
                &process,
            )
            .unwrap_err();

            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }
}
