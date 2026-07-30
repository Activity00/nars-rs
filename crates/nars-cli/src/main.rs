use std::env;
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nars_core::config::OCCURRENCE_ETERNAL;
use nars_core::{
    Answer, Event, EventType, InputOutcome, OperationFeedback, Reasoner, Term, UdpNar,
    process_input,
};

const SHELL_OPERATIONS: [&str; 10] = [
    "^left",
    "^right",
    "^up",
    "^down",
    "^say",
    "^pick",
    "^drop",
    "^go",
    "^activate",
    "^deactivate",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        None | Some("shell") => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            run_shell(stdin.lock(), stdout.lock())
        }
        Some("UDPNAR") => run_udp_nar(&arguments[1..]),
        Some("-h" | "--help" | "help") => {
            print_help(io::stdout().lock())?;
            Ok(())
        }
        Some(command) => Err(format!("unknown command: {command}").into()),
    }
}

type ExecutionLog = Arc<Mutex<Vec<(usize, Term)>>>;

fn shell_reasoner(executions: &ExecutionLog) -> Result<Reasoner, Box<dyn Error>> {
    let mut reasoner = Reasoner::new()?;
    for (index, name) in SHELL_OPERATIONS.into_iter().enumerate() {
        let executions = Arc::clone(executions);
        reasoner.add_operation(name, move |arguments| {
            if let Ok(mut log) = executions.lock() {
                log.push((index + 1, arguments));
            }
            OperationFeedback::default()
        })?;
    }
    Ok(reasoner)
}

fn run_shell<R: BufRead, W: Write>(reader: R, mut output: W) -> Result<(), Box<dyn Error>> {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let mut reasoner = shell_reasoner(&executions)?;
    for line in reader.lines() {
        let line = line?;
        if line.trim() == "quit" {
            break;
        }
        process_shell_line(&mut reasoner, &executions, &line, &mut output)?;
    }
    Ok(())
}

fn process_shell_line<W: Write>(
    reasoner: &mut Reasoner,
    executions: &ExecutionLog,
    line: &str,
    output: &mut W,
) -> Result<(), Box<dyn Error>> {
    match process_input(reasoner, line)? {
        InputOutcome::Ignored => {
            if let Some(comment) = line.trim().strip_prefix("//") {
                writeln!(output, "Comment: {comment}")?;
            }
        }
        InputOutcome::Reset => {
            executions
                .lock()
                .map_err(|_| io::Error::other("operation execution log was poisoned"))?
                .clear();
            *reasoner = shell_reasoner(executions)?;
            writeln!(output, "//NAR reset")?;
        }
        InputOutcome::Cycled(cycles) => {
            writeln!(output, "done with {cycles} additional inference steps.")?;
        }
        InputOutcome::Added(event) => print_event(reasoner, event, output)?,
        InputOutcome::Answered(answer) => print_answer(reasoner, answer, output)?,
        InputOutcome::Configured => writeln!(output, "//configured")?,
        InputOutcome::Inspection(inspection) => writeln!(output, "{inspection}")?,
    }
    print_operation_executions(reasoner, executions, output)?;
    output.flush()?;
    Ok(())
}

fn print_operation_executions<W: Write>(
    reasoner: &Reasoner,
    executions: &ExecutionLog,
    output: &mut W,
) -> io::Result<()> {
    let executions = std::mem::take(
        &mut *executions
            .lock()
            .map_err(|_| io::Error::other("operation execution log was poisoned"))?,
    );
    if executions.is_empty() {
        return Ok(());
    }
    let config = reasoner.operation_config_snapshot();
    for (slot, arguments) in executions {
        let Some(operation) = config
            .operations
            .iter()
            .find(|operation| operation.slot == slot)
        else {
            continue;
        };
        if !operation.stdin_output {
            continue;
        }
        write!(output, "{} executed with args", operation.name)?;
        if !arguments.is_empty() {
            write!(output, " {}", reasoner.language.format_term(&arguments))?;
        }
        writeln!(output)?;
    }
    Ok(())
}

fn print_event<W: Write>(reasoner: &Reasoner, event: Event, output: &mut W) -> io::Result<()> {
    let punctuation = match event.event_type {
        EventType::Belief => '.',
        EventType::Goal => '!',
        EventType::Deleted => '?',
    };
    write!(
        output,
        "Input: {}{}",
        reasoner.language.format_term(&event.term),
        punctuation
    )?;
    if event.occurrence_time != OCCURRENCE_ETERNAL {
        write!(output, " :|: occurrenceTime={}", event.occurrence_time)?;
    }
    writeln!(
        output,
        " Truth: frequency={:.6}, confidence={:.6}",
        event.truth.frequency, event.truth.confidence
    )
}

fn print_answer<W: Write>(
    reasoner: &Reasoner,
    answer: Option<Answer>,
    output: &mut W,
) -> io::Result<()> {
    let Some(answer) = answer else {
        return writeln!(output, "Answer: None.");
    };
    write!(
        output,
        "Answer: {}.",
        reasoner.language.format_term(&answer.term)
    )?;
    if answer.occurrence_time != OCCURRENCE_ETERNAL {
        write!(output, " :|: occurrenceTime={}", answer.occurrence_time)?;
    }
    writeln!(
        output,
        " creationTime={} Truth: frequency={:.6}, confidence={:.6}",
        answer.creation_time, answer.truth.frequency, answer.truth.confidence
    )
}

fn run_udp_nar(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    if arguments.len() < 3 {
        return Err("usage: nars-cli UDPNAR IP PORT timestep_ns [printDerivations]".into());
    }
    let address: SocketAddr = format!("{}:{}", arguments[0], arguments[1]).parse()?;
    let timestep = Duration::from_nanos(arguments[2].parse()?);
    let executions = Arc::new(Mutex::new(Vec::new()));
    let reasoner = shell_reasoner(&executions)?;
    let mut udp_nar = UdpNar::start_with_reasoner(address, timestep, reasoner)?;
    println!("//UDPNAR started on {}", udp_nar.local_addr());

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim() == "quit" {
            break;
        }
        let mut reasoner = udp_nar.lock_reasoner()?;
        process_shell_line(&mut reasoner, &executions, &line, &mut output)?;
    }
    udp_nar.stop()?;
    Ok(())
}

fn print_help<W: Write>(mut output: W) -> io::Result<()> {
    writeln!(output, "nars-cli [shell]")?;
    writeln!(
        output,
        "nars-cli UDPNAR IP PORT timestep_ns [printDerivations]"
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::run_shell;

    #[test]
    fn shell_accepts_beliefs_cycles_and_questions() {
        let input = Cursor::new(b"<cat --> animal>.\n2\n<?what --> animal>?\nquit\n");
        let mut output = Vec::new();
        run_shell(input, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("done with 2 additional inference steps."));
        assert!(output.contains("Answer: <cat --> animal>."));
    }

    #[test]
    fn shell_reset_clears_memory() {
        let input = Cursor::new(b"<cat --> animal>.\n*reset\n<?what --> animal>?\nquit\n");
        let mut output = Vec::new();
        run_shell(input, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("//NAR reset"));
        assert!(output.contains("Answer: None."));
    }

    #[test]
    fn shell_renders_structured_inspection_commands() {
        let input = Cursor::new(b"<cat --> animal>.\n*stats\n*concepts\n*opconfig\nquit\n");
        let mut output = Vec::new();
        run_shell(input, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("//*stats"));
        assert!(output.contains("//*concepts"));
        assert!(output.contains("*setopname 1 ^left"));
    }

    #[test]
    fn shell_prints_ona_operation_execution_format() {
        let input = Cursor::new(
            b"*motorbabbling=false\n*setopname 1 ^act\n<(state &/ ^act) =/> goal>.\nstate. :|:\ngoal! :|:\nquit\n",
        );
        let mut output = Vec::new();
        run_shell(input, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("^act executed with args\n"), "{output}");
    }
}
