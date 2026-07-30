use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use nars_core::{OperationFeedback, Reasoner, ReasonerError};

fn configured_agent() -> Result<Reasoner, ReasonerError> {
    let mut agent = Reasoner::new()?;
    agent.set_motor_babbling_chance(0.0);
    agent.add_operation("^act", |_| OperationFeedback::default())?;
    agent.add_sentence("<(state &/ ^act) =/> goal>.")?;
    Ok(agent)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let idle_cycles: u32 = arguments
        .next()
        .map_or(Ok(1_000_000), |value| value.parse())?;
    let agent_steps: u32 = arguments.next().map_or(Ok(10_000), |value| value.parse())?;

    let mut idle = Reasoner::new()?;
    let started = Instant::now();
    idle.cycles(usize::try_from(idle_cycles)?);
    let idle_elapsed = started.elapsed();
    black_box(idle.current_time());

    let mut text_agent = configured_agent()?;
    let started = Instant::now();
    for _ in 0..agent_steps {
        text_agent.add_sentence("state. :|:")?;
        text_agent.add_sentence("goal! :|:")?;
    }
    let text_agent_elapsed = started.elapsed();
    black_box(text_agent.current_time());

    let mut prepared_agent = configured_agent()?;
    let state = prepared_agent.language.parse_sentence("state. :|:")?;
    let goal = prepared_agent.language.parse_sentence("goal! :|:")?;
    let started = Instant::now();
    for _ in 0..agent_steps {
        prepared_agent.add_parsed_sentence(state)?;
        prepared_agent.add_parsed_sentence(goal)?;
    }
    let prepared_agent_elapsed = started.elapsed();
    black_box(prepared_agent.current_time());

    let mut belief_agent = configured_agent()?;
    let state = belief_agent.language.parse_sentence("state. :|:")?;
    let started = Instant::now();
    for _ in 0..agent_steps {
        belief_agent.add_parsed_sentence(state)?;
    }
    let belief_agent_elapsed = started.elapsed();
    black_box(belief_agent.current_time());

    let mut goal_agent = configured_agent()?;
    let goal = goal_agent.language.parse_sentence("goal! :|:")?;
    let started = Instant::now();
    for _ in 0..agent_steps {
        goal_agent.add_parsed_sentence(goal)?;
    }
    let goal_agent_elapsed = started.elapsed();
    black_box(goal_agent.current_time());

    println!(
        "idle_cycles={idle_cycles} elapsed_seconds={:.6} cycles_per_second={:.0}",
        idle_elapsed.as_secs_f64(),
        f64::from(idle_cycles) / idle_elapsed.as_secs_f64()
    );
    println!(
        "text_agent_steps={agent_steps} elapsed_seconds={:.6} steps_per_second={:.0}",
        text_agent_elapsed.as_secs_f64(),
        f64::from(agent_steps) / text_agent_elapsed.as_secs_f64()
    );
    println!(
        "prepared_agent_steps={agent_steps} elapsed_seconds={:.6} steps_per_second={:.0} concepts={}",
        prepared_agent_elapsed.as_secs_f64(),
        f64::from(agent_steps) / prepared_agent_elapsed.as_secs_f64(),
        prepared_agent.memory.len()
    );
    println!(
        "belief_inputs={agent_steps} elapsed_seconds={:.6} inputs_per_second={:.0}",
        belief_agent_elapsed.as_secs_f64(),
        f64::from(agent_steps) / belief_agent_elapsed.as_secs_f64()
    );
    println!(
        "goal_inputs={agent_steps} elapsed_seconds={:.6} inputs_per_second={:.0}",
        goal_agent_elapsed.as_secs_f64(),
        f64::from(agent_steps) / goal_agent_elapsed.as_secs_f64()
    );
    Ok(())
}
