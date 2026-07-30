//! UDP transport and continuously cycling network reasoner compatibility.

use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::str;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::NARSESE_LEN_MAX;
use crate::{Answer, Event, Punctuation, Reasoner, ReasonerError, ShellInspection, Stamp};

const RECEIVE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GRAPHITE_MAX_MSG_LEN: usize = 130;

/// A bound UDP socket that preserves datagrams exactly as received.
#[derive(Debug)]
pub struct UdpReceiver {
    socket: UdpSocket,
}

impl UdpReceiver {
    /// Binds a UDP receiver.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when address resolution or binding fails.
    pub fn bind<A: ToSocketAddrs>(address: A) -> io::Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind(address)?,
        })
    }

    /// Receives one complete datagram into `buffer`.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn receive(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buffer)
    }

    /// Returns the address selected by the operating system.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Configures blocking receive timeout behavior.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    /// Configures nonblocking mode.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.socket.set_nonblocking(nonblocking)
    }
}

/// An unconnected IPv4 UDP sender, equivalent to ONA's `UDP_INIT_Sender`.
#[derive(Debug)]
pub struct UdpSender {
    socket: UdpSocket,
}

impl UdpSender {
    /// Creates a sender on an ephemeral local port.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the socket cannot be created.
    pub fn new() -> io::Result<Self> {
        Self::bind((Ipv4Addr::UNSPECIFIED, 0))
    }

    /// Creates a sender using an explicit local address.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when address resolution or binding fails.
    pub fn bind<A: ToSocketAddrs>(address: A) -> io::Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind(address)?,
        })
    }

    /// Sends exactly the supplied bytes as one UDP datagram.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn send_to<A: ToSocketAddrs>(&self, bytes: &[u8], address: A) -> io::Result<usize> {
        self.socket.send_to(bytes, address)
    }

    /// Sends a UTF-8 sentence followed by ONA's conventional trailing NUL.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn send_narsese_to<A: ToSocketAddrs>(
        &self,
        sentence: &str,
        address: A,
    ) -> io::Result<usize> {
        let mut bytes = Vec::with_capacity(sentence.len() + 1);
        bytes.extend_from_slice(sentence.as_bytes());
        bytes.push(0);
        self.send_to(&bytes, address)
    }
}

/// Result of processing one ONA shell-compatible input line.
#[derive(Clone, Debug, PartialEq)]
pub enum InputOutcome {
    Ignored,
    Reset,
    Cycled(usize),
    Added(Event),
    Answered(Option<Answer>),
    Configured,
    Inspection(Box<ShellInspection>),
}

/// Processes the shell subset needed by network and embedded applications.
///
/// Empty input advances one cycle, decimal integers advance that many cycles,
/// comments are ignored, `*reset` is returned to the owner, and other input is
/// parsed as a complete Narsese sentence.
///
/// # Errors
///
/// Returns a reasoner error for malformed or unsupported Narsese.
#[allow(clippy::too_many_lines)]
pub fn process_input(reasoner: &mut Reasoner, input: &str) -> Result<InputOutcome, ReasonerError> {
    let input = input.trim();
    if input.is_empty() {
        reasoner.cycles(1);
        return Ok(InputOutcome::Cycled(1));
    }
    if input.starts_with("//") {
        return Ok(InputOutcome::Ignored);
    }
    if input == "*reset" {
        return Ok(InputOutcome::Reset);
    }
    if input == "*stats" {
        return Ok(InputOutcome::Inspection(Box::new(ShellInspection::Stats(
            reasoner.stats_snapshot(),
        ))));
    }
    if input == "*concepts" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::Concepts(reasoner.concepts_snapshot()),
        )));
    }
    if input == "*cycling_belief_events" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::BeliefQueue(reasoner.belief_queue_snapshot()),
        )));
    }
    if input == "*cycling_goal_events" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::GoalQueue(reasoner.goal_queue_snapshot()),
        )));
    }
    if input == "*inverted_atom_index" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::InvertedAtomIndex(reasoner.inverted_atom_index_snapshot()),
        )));
    }
    if input == "*occurrence_time_index" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::OccurrenceTimeIndex(reasoner.occurrence_time_index_snapshot()),
        )));
    }
    if input == "*opconfig" {
        return Ok(InputOutcome::Inspection(Box::new(
            ShellInspection::OperationConfig(reasoner.operation_config_snapshot()),
        )));
    }
    if let Some(value) = input.strip_prefix("*volume=") {
        let _ = parse_f64(value, "output volume")?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*restrictedconceptcreation=") {
        let restricted = parse_bool(value)?;
        reasoner.set_restricted_concept_creation(restricted);
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*anticipationconfidence=") {
        reasoner.set_anticipation_confidence(parse_f64(value, "anticipation confidence")?)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*decisionthreshold=") {
        reasoner.set_decision_threshold(parse_f64(value, "decision threshold")?)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*similaritydistance=") {
        reasoner.set_similarity_distance(parse_f64(value, "similarity distance")?)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*babblingops=") {
        reasoner.set_babbling_operations(parse_usize(value, "babbling operation count")?)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*currenttime=") {
        reasoner.set_current_time(
            value
                .parse()
                .map_err(|_| ReasonerError::new("invalid current time"))?,
        );
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*stampid=") {
        reasoner.set_next_evidence_id(
            value
                .parse()
                .map_err(|_| ReasonerError::new("invalid evidence stamp ID"))?,
        );
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*stampimport=[") {
        let values = value
            .strip_suffix(']')
            .ok_or_else(|| ReasonerError::new("stamp import is missing closing bracket"))?
            .split(',')
            .filter(|field| !field.trim().is_empty())
            .map(|field| {
                field
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| ReasonerError::new("invalid imported stamp evidence"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        reasoner.set_import_stamp(Stamp::from_evidence(values));
        return Ok(InputOutcome::Configured);
    }
    if input == "*motorbabbling=false" {
        reasoner.set_motor_babbling_chance(0.0);
        return Ok(InputOutcome::Configured);
    }
    if input == "*motorbabbling=true" {
        reasoner.set_motor_babbling_chance(crate::config::MOTOR_BABBLING_CHANCE);
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*motorbabbling=") {
        let chance = parse_f64(value, "motor babbling chance")?;
        if !(0.0..=1.0).contains(&chance) {
            return Err(ReasonerError::new(
                "motor babbling chance must be between 0 and 1",
            ));
        }
        reasoner.set_motor_babbling_chance(chance);
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*questionpriming=") {
        reasoner.set_question_priming(parse_f64(value, "question priming")?)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(arguments) = input.strip_prefix("*setvalue ") {
        let mut fields = arguments.split_whitespace();
        let value = parse_f64(
            fields
                .next()
                .ok_or_else(|| ReasonerError::new("setvalue requires a value"))?,
            "numeric value",
        )?;
        let granularity = parse_usize(
            fields
                .next()
                .ok_or_else(|| ReasonerError::new("setvalue requires granularity"))?,
            "numeric granularity",
        )?;
        let measurement = fields.next().unwrap_or_default();
        reasoner.set_numeric_value(value, granularity, measurement)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(arguments) = input.strip_prefix("*space ") {
        let mut fields = arguments.split_whitespace();
        let granularity = parse_usize(
            fields
                .next()
                .ok_or_else(|| ReasonerError::new("space requires granularity"))?,
            "numeric granularity",
        )?;
        let measurement = fields.next().unwrap_or_default();
        reasoner.define_numeric_space(granularity, measurement)?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(arguments) = input.strip_prefix("*setopname ") {
        let (slot, name) = arguments
            .split_once(char::is_whitespace)
            .ok_or_else(|| ReasonerError::new("setopname requires slot and name"))?;
        reasoner.set_operation_name(parse_usize(slot, "operation slot")?, name.trim())?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(arguments) = input.strip_prefix("*setoparg ") {
        let mut fields = arguments.splitn(3, char::is_whitespace);
        let operation_slot = parse_usize(
            fields
                .next()
                .ok_or_else(|| ReasonerError::new("setoparg requires operation slot"))?,
            "operation slot",
        )?;
        let argument_slot = parse_usize(
            fields
                .next()
                .ok_or_else(|| ReasonerError::new("setoparg requires argument slot"))?,
            "operation argument slot",
        )?;
        let term = fields
            .next()
            .ok_or_else(|| ReasonerError::new("setoparg requires an argument term"))?;
        reasoner.set_operation_argument(operation_slot, argument_slot, term.trim())?;
        return Ok(InputOutcome::Configured);
    }
    if let Some(value) = input.strip_prefix("*setopstdin ") {
        reasoner.set_operation_stdin(parse_usize(value, "operation slot")?, true)?;
        return Ok(InputOutcome::Configured);
    }
    if input.starts_with("*concurrent") {
        reasoner.set_current_time(reasoner.current_time().saturating_sub(1));
        return Ok(InputOutcome::Configured);
    }
    if let Some(arguments) = input.strip_prefix("*query ") {
        let (threshold, question) = arguments
            .split_once(char::is_whitespace)
            .ok_or_else(|| ReasonerError::new("query requires threshold and question"))?;
        let threshold = parse_f64(threshold, "query threshold")?;
        if !(0.0..=1.0).contains(&threshold) {
            return Err(ReasonerError::new(
                "query threshold must be between 0 and 1",
            ));
        }
        let answer = reasoner
            .ask(question.trim())?
            .filter(|answer| answer.truth.expectation() > threshold);
        return Ok(InputOutcome::Answered(answer));
    }
    if input.bytes().all(|byte| byte.is_ascii_digit()) {
        let cycles = input
            .parse::<usize>()
            .map_err(|_| ReasonerError::new("cycle count is too large"))?;
        reasoner.cycles(cycles);
        return Ok(InputOutcome::Cycled(cycles));
    }
    let punctuation = reasoner
        .language
        .parse_sentence(input)
        .map_err(|error| ReasonerError::new(error.to_string()))?
        .punctuation;
    if punctuation == Punctuation::Question {
        reasoner.ask(input).map(InputOutcome::Answered)
    } else {
        reasoner.add_sentence(input).map(InputOutcome::Added)
    }
}

fn parse_bool(value: &str) -> Result<bool, ReasonerError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ReasonerError::new("boolean setting must be true or false")),
    }
}

fn parse_f64(value: &str, name: &str) -> Result<f64, ReasonerError> {
    value
        .trim()
        .parse()
        .map_err(|_| ReasonerError::new(format!("invalid {name}")))
}

fn parse_usize(value: &str, name: &str) -> Result<usize, ReasonerError> {
    value
        .trim()
        .parse()
        .map_err(|_| ReasonerError::new(format!("invalid {name}")))
}

#[derive(Debug)]
pub enum UdpNarError {
    Io(io::Error),
    Reasoner(ReasonerError),
    Poisoned,
    ThreadPanicked,
}

impl fmt::Display for UdpNarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "UDP I/O error: {error}"),
            Self::Reasoner(error) => write!(formatter, "reasoner input error: {error}"),
            Self::Poisoned => formatter.write_str("reasoner mutex was poisoned"),
            Self::ThreadPanicked => formatter.write_str("UDPNAR background thread panicked"),
        }
    }
}

impl Error for UdpNarError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Reasoner(error) => Some(error),
            Self::Poisoned | Self::ThreadPanicked => None,
        }
    }
}

impl From<io::Error> for UdpNarError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ReasonerError> for UdpNarError {
    fn from(error: ReasonerError) -> Self {
        Self::Reasoner(error)
    }
}

/// A continuously cycling reasoner receiving shell input through UDP.
pub struct UdpNar {
    reasoner: Arc<Mutex<Reasoner>>,
    socket: UdpSocket,
    local_addr: SocketAddr,
    stopped: Arc<AtomicBool>,
    cycle_stop: Option<Sender<()>>,
    cycle_thread: Option<JoinHandle<Result<(), UdpNarError>>>,
    receive_thread: Option<JoinHandle<Result<(), UdpNarError>>>,
}

impl fmt::Debug for UdpNar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UdpNar")
            .field("local_addr", &self.local_addr)
            .field("stopped", &self.stopped.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl UdpNar {
    /// Starts a fresh reasoner bound to a UDP address.
    ///
    /// # Errors
    ///
    /// Returns an error if the reasoner cannot initialize or the socket cannot
    /// bind or be configured.
    pub fn start<A: ToSocketAddrs>(address: A, timestep: Duration) -> Result<Self, UdpNarError> {
        Self::start_with_reasoner(address, timestep, Reasoner::new()?)
    }

    /// Starts a caller-configured reasoner bound to a UDP address.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot bind, clone, or be configured.
    pub fn start_with_reasoner<A: ToSocketAddrs>(
        address: A,
        timestep: Duration,
        reasoner: Reasoner,
    ) -> Result<Self, UdpNarError> {
        let socket = UdpSocket::bind(address)?;
        socket.set_read_timeout(Some(RECEIVE_POLL_INTERVAL))?;
        let local_addr = socket.local_addr()?;
        let receive_socket = socket.try_clone()?;
        let reasoner = Arc::new(Mutex::new(reasoner));
        let stopped = Arc::new(AtomicBool::new(false));
        let (cycle_stop, cycle_stop_receiver) = mpsc::channel();

        let cycle_reasoner = Arc::clone(&reasoner);
        let cycle_stopped = Arc::clone(&stopped);
        let cycle_thread = thread::spawn(move || {
            while !cycle_stopped.load(Ordering::Acquire) {
                cycle_reasoner
                    .lock()
                    .map_err(|_| UdpNarError::Poisoned)?
                    .cycle();
                match cycle_stop_receiver.recv_timeout(timestep) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            Ok(())
        });

        let receive_reasoner = Arc::clone(&reasoner);
        let receive_stopped = Arc::clone(&stopped);
        let receive_thread = thread::spawn(move || {
            let mut buffer = [0_u8; NARSESE_LEN_MAX];
            while !receive_stopped.load(Ordering::Acquire) {
                let size = match receive_socket.recv_from(&mut buffer) {
                    Ok((size, _)) => size,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(UdpNarError::Io(error)),
                };
                if receive_stopped.load(Ordering::Acquire) {
                    break;
                }
                let bytes = &buffer[..size];
                let text_end = bytes.iter().position(|byte| *byte == 0).unwrap_or(size);
                let input = str::from_utf8(&bytes[..text_end]).map_err(|error| {
                    UdpNarError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
                })?;
                let mut guard = receive_reasoner.lock().map_err(|_| UdpNarError::Poisoned)?;
                if process_input(&mut guard, input)? == InputOutcome::Reset {
                    *guard = Reasoner::new()?;
                }
            }
            Ok(())
        });

        Ok(Self {
            reasoner,
            socket,
            local_addr,
            stopped,
            cycle_stop: Some(cycle_stop),
            cycle_thread: Some(cycle_thread),
            receive_thread: Some(receive_thread),
        })
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[must_use]
    pub fn reasoner(&self) -> Arc<Mutex<Reasoner>> {
        Arc::clone(&self.reasoner)
    }

    /// Locks the reasoner for operation registration or state inspection.
    ///
    /// # Errors
    ///
    /// Returns an error if a background panic poisoned the mutex.
    pub fn lock_reasoner(&self) -> Result<MutexGuard<'_, Reasoner>, UdpNarError> {
        self.reasoner.lock().map_err(|_| UdpNarError::Poisoned)
    }

    /// Stops both background threads and waits for clean termination.
    ///
    /// Calling this method more than once is harmless.
    ///
    /// # Errors
    ///
    /// Returns a background I/O/reasoner error or a thread panic.
    pub fn stop(&mut self) -> Result<(), UdpNarError> {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Some(sender) = self.cycle_stop.take() {
            let _ = sender.send(());
        }
        let _ = self.socket.send_to(&[], wake_address(self.local_addr));
        let cycle_result = join_thread(self.cycle_thread.take());
        let receive_result = join_thread(self.receive_thread.take());
        cycle_result.and(receive_result)
    }
}

impl Drop for UdpNar {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn wake_address(address: SocketAddr) -> SocketAddr {
    if !address.ip().is_unspecified() {
        return address;
    }
    let ip = match address.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    SocketAddr::new(ip, address.port())
}

fn join_thread(thread: Option<JoinHandle<Result<(), UdpNarError>>>) -> Result<(), UdpNarError> {
    match thread {
        Some(thread) => thread.join().map_err(|_| UdpNarError::ThreadPanicked)?,
        None => Ok(()),
    }
}

/// Minimal `StatsD` counter client matching ONA's Graphite metric format.
#[derive(Debug)]
pub struct MetricClient {
    sender: UdpSender,
    destination: SocketAddr,
}

impl MetricClient {
    /// Creates a counter client for one `StatsD` destination.
    ///
    /// # Errors
    ///
    /// Returns an error if the destination cannot be resolved or a sender
    /// socket cannot be created.
    pub fn new<A: ToSocketAddrs>(destination: A) -> io::Result<Self> {
        let destination = destination
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no UDP destination"))?;
        let sender = match destination {
            SocketAddr::V4(_) => UdpSender::new()?,
            SocketAddr::V6(_) => UdpSender::bind((Ipv6Addr::UNSPECIFIED, 0))?,
        };
        Ok(Self {
            sender,
            destination,
        })
    }

    /// Sends `<path>:<value>|c` as a single UDP datagram.
    ///
    /// # Errors
    ///
    /// Returns an error for a message that exceeds ONA's 130-byte metric
    /// buffer or for an underlying socket failure.
    pub fn send_counter(&self, path: &str, value: i64) -> io::Result<usize> {
        let message = encode_counter(path, value)?;
        self.sender.send_to(message.as_bytes(), self.destination)
    }
}

/// Encodes ONA's `<path>:<value>|c` `StatsD` counter representation.
///
/// # Errors
///
/// Returns an error when the encoded message does not fit ONA's 130-byte
/// Graphite buffer.
pub fn encode_counter(path: &str, value: i64) -> io::Result<String> {
    let message = format!("{path}:{value}|c");
    if message.len() >= GRAPHITE_MAX_MSG_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metric message exceeds ONA's Graphite buffer",
        ));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        InputOutcome, MetricClient, UdpNar, UdpReceiver, UdpSender, encode_counter, process_input,
    };
    use crate::{OperationFeedback, Reasoner, Stamp};

    #[test]
    fn udp_preserves_nul_terminated_narsese() {
        let receiver = UdpReceiver::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let sender = UdpSender::new().unwrap();
        let destination = receiver.local_addr().unwrap();
        let payload = b"<(a &/ ^left) =/> g>.\0";
        assert_eq!(sender.send_to(payload, destination).unwrap(), payload.len());

        let mut datagram = [0_u8; 128];
        let (size, _) = receiver.receive(&mut datagram).unwrap();
        assert_eq!(&datagram[..size], payload);
    }

    #[test]
    fn metric_uses_upstream_statsd_counter_format() {
        assert_eq!(
            encode_counter("reasoner.cycles", 42).unwrap(),
            "reasoner.cycles:42|c"
        );

        let receiver = UdpReceiver::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let metric = MetricClient::new(receiver.local_addr().unwrap()).unwrap();
        metric.send_counter("reasoner.cycles", 42).unwrap();
        let mut datagram = [0_u8; 128];
        let (size, _) = receiver.receive(&mut datagram).unwrap();
        assert_eq!(&datagram[..size], b"reasoner.cycles:42|c");
    }

    #[test]
    fn shell_input_dispatches_questions() {
        let mut reasoner = Reasoner::new().unwrap();
        process_input(&mut reasoner, "<cat --> animal>.").unwrap();
        let outcome = process_input(&mut reasoner, "<?what --> animal>?").unwrap();
        let InputOutcome::Answered(Some(answer)) = outcome else {
            panic!("question did not produce an answer");
        };
        assert_eq!(
            reasoner.language.format_term(&answer.term),
            "<cat --> animal>"
        );
    }

    #[test]
    fn shell_configuration_changes_runtime_behavior() {
        let captured_argument = Arc::new(Mutex::new(None));
        let callback_capture = Arc::clone(&captured_argument);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |argument| {
                *callback_capture.lock().unwrap() = Some(argument);
                OperationFeedback::default()
            })
            .unwrap();

        for command in [
            "*volume=100",
            "*motorbabbling=1",
            "*babblingops=1",
            "*decisionthreshold=0.75",
            "*anticipationconfidence=0.2",
            "*questionpriming=0.8",
            "*similaritydistance=2.5",
            "*restrictedconceptcreation=true",
            "*currenttime=40",
            "*stampid=99",
            "*stampimport=[7,8]",
            "*setvalue 0.5 100 temperature",
            "*setopname 1 ^move",
            "*setoparg 1 1 bedroom",
            "*setopstdin 1",
        ] {
            assert_eq!(
                process_input(&mut reasoner, command).unwrap(),
                InputOutcome::Configured,
                "{command}"
            );
        }
        assert!((reasoner.motor_babbling_chance() - 1.0).abs() < f64::EPSILON);
        assert_eq!(reasoner.babbling_operations(), 1);
        assert!((reasoner.decision_threshold() - 0.75).abs() < f64::EPSILON);
        assert!((reasoner.anticipation_confidence() - 0.2).abs() < f64::EPSILON);
        assert!((reasoner.question_priming() - 0.8).abs() < f64::EPSILON);
        assert!((reasoner.similarity_distance() - 2.5).abs() < f64::EPSILON);
        assert!(reasoner.memory.restricted_creation);

        let InputOutcome::Added(first) = process_input(&mut reasoner, "state. :|:").unwrap() else {
            panic!("belief was not added");
        };
        assert_eq!(first.occurrence_time, 40);
        assert_eq!(first.stamp, Stamp::from_evidence([7, 8]));
        let temperature = reasoner.language.parse_term("temperature_0.50").unwrap();
        assert_eq!(
            reasoner.language.atom_value(temperature.root()),
            Some((0.5, "temperature"))
        );
        reasoner.cycles(3);
        let state = reasoner.language.parse_term("state").unwrap();
        assert!(reasoner.memory.find(&state).unwrap().priority < 0.8);
        process_input(&mut reasoner, "state? :|:").unwrap();
        assert!(reasoner.memory.find(&state).unwrap().priority >= 0.8);

        let InputOutcome::Added(goal) = process_input(&mut reasoner, "unknown! :|:").unwrap()
        else {
            panic!("goal was not added");
        };
        assert_eq!(goal.stamp, Stamp::from_evidence([99]));
        let argument = captured_argument
            .lock()
            .unwrap()
            .expect("configured operation was not explored");
        assert_eq!(
            reasoner.language.format_term(&argument),
            "({SELF} * bedroom)"
        );
    }

    #[test]
    fn shell_query_threshold_filters_best_answer() {
        let mut reasoner = Reasoner::new().unwrap();
        process_input(&mut reasoner, "<cat --> animal>.").unwrap();
        assert_eq!(
            process_input(&mut reasoner, "*query 0.99 <?what --> animal>?").unwrap(),
            InputOutcome::Answered(None)
        );
        assert!(matches!(
            process_input(&mut reasoner, "*query 0.5 <?what --> animal>?").unwrap(),
            InputOutcome::Answered(Some(_))
        ));
    }

    #[test]
    fn inspection_commands_return_consistent_structured_snapshots() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", |_| OperationFeedback::default())
            .unwrap();
        process_input(&mut reasoner, "<cat --> animal>.").unwrap();
        process_input(&mut reasoner, "state. :|:").unwrap();

        let stats = reasoner.stats_snapshot();
        assert!(stats.total_concepts >= 2);
        assert_eq!(stats.operations, 1);
        assert_eq!(stats.current_time, reasoner.current_time());

        let concepts = reasoner.concepts_snapshot();
        assert!(
            concepts
                .iter()
                .any(|concept| { concept.term == "<cat --> animal>" && concept.belief.is_some() })
        );
        assert!(
            reasoner
                .occurrence_time_index_snapshot()
                .iter()
                .any(|entry| entry.concept == "state")
        );
        assert!(reasoner.inverted_atom_index_snapshot().iter().any(|entry| {
            entry.atom == "cat"
                && entry
                    .concepts
                    .iter()
                    .any(|concept| concept == "<cat --> animal>")
        }));
        let operations = reasoner.operation_config_snapshot();
        assert_eq!(operations.operations[0].name, "^left");

        for (command, marker) in [
            ("*stats", "//*stats"),
            ("*concepts", "//*concepts"),
            ("*cycling_belief_events", "//*cycling_belief_events"),
            ("*cycling_goal_events", "//*cycling_goal_events"),
            ("*inverted_atom_index", "//*inverted_atom_index"),
            ("*occurrence_time_index", "//*occurrence_time_index"),
            ("*opconfig", "//*opconfig"),
        ] {
            let InputOutcome::Inspection(snapshot) = process_input(&mut reasoner, command).unwrap()
            else {
                panic!("{command} did not return an inspection snapshot");
            };
            assert!(snapshot.to_string().contains(marker), "{command}");
        }
    }

    #[test]
    fn udpnar_executes_learned_operation() {
        let executed = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&executed);
        let mut udp_nar =
            UdpNar::start((Ipv4Addr::LOCALHOST, 0), Duration::from_millis(10)).unwrap();
        udp_nar
            .lock_reasoner()
            .unwrap()
            .add_operation("^left", move |_| {
                callback_flag.store(true, Ordering::Release);
                OperationFeedback::default()
            })
            .unwrap();

        let sender = UdpSender::new().unwrap();
        let destination = udp_nar.local_addr();
        sender
            .send_narsese_to("<(a &/ ^left) =/> g>.", destination)
            .unwrap();
        sender.send_narsese_to("a. :|:", destination).unwrap();
        sender.send_narsese_to("g! :|:", destination).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while !executed.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(executed.load(Ordering::Acquire));
        udp_nar.stop().unwrap();
    }
}
