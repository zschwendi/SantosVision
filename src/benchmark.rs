//! Optional, host-only scene pin for disposable performance runs.
//! All offsets and exports mirror the existing mp::script_tick/Cmd contract.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use wasmtime::{Caller, Val};
use crate::{read_cstr, read_u32, read_u64, write_bytes, write_u32, write_u64, HostState, RT};

const THREAD_NAME: u64 = 428;        // mp::THREAD_NAME
const ACTIVE_THREAD_TLS: u64 = 3088; // mp::ACTIVE_THREAD_TLS
const SCRATCH: i64 = 2048;           // mp::SCRATCH
const VECTOR_ARG: u64 = 32;          // mp::Cmd::vec_arg(slot 1)
const WEATHER_NAME: u64 = 1520;      // mp::S_NAME
const FIXTURE_ID: &str = "airport_clear_noon_v1";
const POSITION: [f32; 3] = [-1037.0, -2738.0, 13.8];
const HEADING: f32 = 90.0;
const CLOCK: [i32; 3] = [12, 0, 0];
const WEATHER: &str = "CLEAR";

enum Request { Off, Fixture, Invalid(String) }
static REQUEST: LazyLock<Request> = LazyLock::new(|| match std::env::var("MUGUET_DIAGNOSTIC_SCENE") {
    Err(std::env::VarError::NotPresent) => Request::Off,
    Ok(value) if value == FIXTURE_ID => Request::Fixture,
    Ok(value) => Request::Invalid(format!("unsupported diagnostic scene fixture: {value}")),
    Err(error) => Request::Invalid(format!("invalid diagnostic scene environment: {error}")),
});

#[derive(Default)]
struct Status {
    ticks: u64,
    applied: u64,
    verified_consecutive: u64,
    last_ped: i32,
    last_position: Option<[f32; 3]>,
    last_heading: Option<f32>,
    last_clock: Option<[i32; 3]>,
    last_weather_hash: Option<u32>,
    last_error: Option<String>,
}
static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| Mutex::new(Status::default()));

fn invoke(caller: &mut Caller<'_, HostState>, name: &str, args: &[Val]) -> Result<Val> {
    let function = caller.get_export(name).and_then(|export| export.into_func())
        .with_context(|| format!("diagnostic scene missing export {name}"))?;
    let ty = function.ty(&mut *caller);
    if ty.params().count() != args.len() { bail!("diagnostic scene ABI argument mismatch: {name}"); }
    let args = crate::coerce_args(args, &ty);
    let mut out: Vec<Val> = ty.results().map(|kind| crate::default_val(&kind)).collect();
    function.call(&mut *caller, &args, &mut out)
        .with_context(|| format!("diagnostic scene command trapped: {name}"))?;
    Ok(out.into_iter().next().unwrap_or(Val::I32(0)))
}

fn integer(value: Val, name: &str) -> Result<i32> {
    match value { Val::I32(value) => Ok(value), Val::I64(value) => Ok(value as i32), _ => Err(anyhow!("diagnostic scene result type mismatch: {name}")) }
}
fn scalar(value: Val, name: &str) -> Result<f32> {
    match value { Val::F32(bits) => Ok(f32::from_bits(bits)), _ => Err(anyhow!("diagnostic scene result type mismatch: {name}")) }
}
fn i(value: i32) -> Val { Val::I32(value) }
fn f(value: f32) -> Val { Val::F32(value.to_bits()) }

fn command(caller: &mut Caller<'_, HostState>, name: &str, args: &[Val]) -> Result<Val> {
    invoke(caller, &format!("mp:{name}"), args)
}

fn write_vector(scratch: u64, value: [f32; 3]) -> Result<Val> {
    let mem = &RT.get().context("missing diagnostic scene runtime")?.mem;
    for (index, component) in value.iter().enumerate() {
        let offset = scratch + VECTOR_ARG + index as u64 * 8;
        if !write_u32(mem, offset, component.to_bits()) || !write_u32(mem, offset + 4, 0) {
            bail!("diagnostic scene scratch vector write failed");
        }
    }
    Ok(Val::I64((scratch + VECTOR_ARG) as i64))
}

fn read_vector(scratch: u64) -> Result<[f32; 3]> {
    let mem = &RT.get().context("missing diagnostic scene runtime")?.mem;
    let mut position = [0.0; 3];
    for (index, offset) in [0u64, 8, 16].into_iter().enumerate() {
        position[index] = f32::from_bits(read_u32(mem, scratch + offset)
            .context("diagnostic scene coordinate readback failed")?);
    }
    Ok(position)
}

fn apply(caller: &mut Caller<'_, HostState>, scratch: u64) -> Result<(i32, [f32; 3], f32, [i32; 3], u32)> {
    let ped = integer(command(caller, "PlayerPedId", &[])?, "PlayerPedId")?;
    if ped == 0 || integer(command(caller, "DoesEntityExist", &[i(ped)])?, "DoesEntityExist")? == 0 {
        bail!("diagnostic scene player ped not ready");
    }
    if integer(command(caller, "IsPedInAnyVehicle", &[i(ped), i(0)])?, "IsPedInAnyVehicle")? != 0 {
        bail!("diagnostic scene player is in a vehicle");
    }
    let position = write_vector(scratch, POSITION)?;
    command(caller, "SetEntityCoordsNoOffset", &[i(ped), position, i(0), i(0), i(0)])?;
    command(caller, "SetEntityHeading", &[i(ped), f(HEADING)])?;
    command(caller, "FreezeEntityPosition", &[i(ped), i(1)])?;
    command(caller, "SetClockTime", &[i(CLOCK[0]), i(CLOCK[1]), i(CLOCK[2])])?;
    let mem = &RT.get().context("missing diagnostic scene runtime")?.mem;
    if !write_bytes(mem, scratch + WEATHER_NAME, b"CLEAR\0") { bail!("diagnostic scene weather name write failed"); }
    command(caller, "SetWeatherTypeNowPersist", &[Val::I64((scratch + WEATHER_NAME) as i64)])?;

    command(caller, "GetEntityCoords", &[Val::I64(scratch as i64), i(ped), i(0)])?;
    let actual_position = read_vector(scratch)?;
    let actual_heading = scalar(command(caller, "GetEntityHeading", &[i(ped)])?, "GetEntityHeading")?;
    let actual_clock = [
        integer(command(caller, "GetClockHours", &[])?, "GetClockHours")?,
        integer(command(caller, "GetClockMinutes", &[])?, "GetClockMinutes")?,
        integer(command(caller, "GetClockSeconds", &[])?, "GetClockSeconds")?,
    ];
    let actual_weather = integer(command(caller, "GetNextWeatherTypeHashName", &[])?, "GetNextWeatherTypeHashName")? as u32;
    Ok((ped, actual_position, actual_heading, actual_clock, actual_weather))
}

fn matches_fixture(position: [f32; 3], heading: f32, clock: [i32; 3], weather: u32) -> bool {
    position.iter().zip(POSITION).all(|(actual, expected)| actual.is_finite() && (*actual - expected).abs() <= 0.25)
        && heading.is_finite() && ((heading - HEADING + 180.0).rem_euclid(360.0) - 180.0).abs() <= 1.0
        && clock[0] == CLOCK[0] && clock[1] == CLOCK[1] && (0..=1).contains(&clock[2])
        && weather == crate::proto::joaat(WEATHER)
}

pub fn tick(caller: &mut Caller<'_, HostState>, thread: u64, tls: u64) {
    if !matches!(&*REQUEST, Request::Fixture) { return; }
    let Some(runtime) = RT.get() else { return; };
    let mem = &runtime.mem;
    if read_cstr(mem, thread + THREAD_NAME) != "main_persistent" { return; }
    let active = tls + ACTIVE_THREAD_TLS;
    let outcome = (|| -> Result<_> {
        let saved = read_u64(mem, active).context("diagnostic scene TLS pointer unreadable")?;
        if !write_u64(mem, active, thread) { bail!("diagnostic scene TLS pointer unwritable"); }
        let stack = invoke(caller, "emscripten_stack_get_current", &[]).and_then(|value| match value {
            Val::I64(value) if value != 0 => Ok(value),
            _ => Err(anyhow!("diagnostic scene stack pointer unavailable")),
        });
        let result = match stack {
            Ok(stack) => {
                let result = (|| {
                    let scratch = match invoke(caller, "_emscripten_stack_alloc", &[Val::I64(SCRATCH)])? {
                        Val::I64(value) if value != 0 => value as u64,
                        _ => bail!("diagnostic scene stack allocation failed"),
                    };
                    apply(caller, scratch)
                })();
                let restored = invoke(caller, "_emscripten_stack_restore", &[Val::I64(stack)]);
                result.and_then(|value| restored.map(|_| value))
            }
            Err(error) => Err(error),
        };
        let restored_tls = write_u64(mem, active, saved);
        if !restored_tls { bail!("diagnostic scene TLS restore failed"); }
        result
    })();
    let mut status = STATUS.lock().unwrap_or_else(|poison| poison.into_inner());
    status.ticks += 1;
    match outcome {
        Ok((ped, position, heading, clock, weather)) => {
            status.applied += 1;
            status.last_ped = ped;
            status.last_position = Some(position);
            status.last_heading = Some(heading);
            status.last_clock = Some(clock);
            status.last_weather_hash = Some(weather);
            if matches_fixture(position, heading, clock, weather) {
                status.verified_consecutive += 1;
                status.last_error = None;
            } else {
                status.verified_consecutive = 0;
                status.last_error = Some("diagnostic scene readback differs from requested fixture".into());
            }
        }
        Err(error) => { status.verified_consecutive = 0; status.last_error = Some(format!("{error:#}")); }
    }
}

pub fn status_json() -> Value {
    let status = STATUS.lock().unwrap_or_else(|poison| poison.into_inner());
    let (requested, error) = match &*REQUEST {
        Request::Off => (None, None),
        Request::Fixture => (Some(FIXTURE_ID), None),
        Request::Invalid(error) => (None, Some(error.as_str())),
    };
    json!({"requested_fixture":requested,"request_error":error,"script":"main_persistent",
        "requested_position":POSITION,"requested_heading":HEADING,"requested_clock":CLOCK,"requested_weather":WEATHER,
        "ticks":status.ticks,"applied":status.applied,"verified_consecutive":status.verified_consecutive,
        "qualified":requested.is_some() && status.verified_consecutive >= 10 && status.last_error.is_none(),
        "last_ped":status.last_ped,"last_position":status.last_position,"last_heading":status.last_heading,
        "last_clock":status.last_clock,"last_weather_hash":status.last_weather_hash,"last_error":status.last_error})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_scene_uses_only_preexisting_qualified_command_exports() {
        for name in ["PlayerPedId", "DoesEntityExist", "IsPedInAnyVehicle", "SetEntityCoordsNoOffset", "GetEntityCoords",
            "SetEntityHeading", "GetEntityHeading", "FreezeEntityPosition", "SetClockTime",
            "GetClockHours", "GetClockMinutes", "GetClockSeconds", "SetWeatherTypeNowPersist",
            "GetNextWeatherTypeHashName"] {
            assert!(crate::mp::COMMANDS.iter().any(|(alias, _)| *alias == name), "missing preexisting {name}");
        }
    }
    #[test]
    fn diagnostic_fixture_readback_is_strict_about_position_heading_clock_and_weather() {
        let weather = crate::proto::joaat(WEATHER);
        assert!(matches_fixture(POSITION, HEADING, CLOCK, weather));
        let mut displaced = POSITION; displaced[0] += 0.26;
        assert!(!matches_fixture(displaced, HEADING, CLOCK, weather));
        assert!(!matches_fixture(POSITION, HEADING + 1.1, CLOCK, weather));
        assert!(!matches_fixture(POSITION, HEADING, [11, 59, 59], weather));
        assert!(!matches_fixture(POSITION, HEADING, CLOCK, weather ^ 1));
        assert!(!matches_fixture([f32::NAN, 0.0, 0.0], HEADING, CLOCK, weather));
    }
}
