//! `erisg1 v0 sensors`: a sensor HUD that runs on the arms themselves. Each arm runs the same program: 25 times a
//! second it draws a level line from its own accelerometer, and every eighth time its own raw sensor readings, in a
//! column on its own side of the view (the left arm's on the left), so the two eyes never show different things in
//! the same place. The Mac only loads it and measures how far the arms' accelerometers disagree, so both lines
//! match; then it can go away.

use std::time::Duration;

use crate::link::Error;
use crate::proto::Side;
use crate::v5::{self, prop};
use crate::v5_asm::*;
use crate::v5_link::{Arm, side_name};

// Where the program keeps things, above the frame and font (v5::FRAME, v5::FONT).
const POOL: u32 = 0x30000;
const VARS: u32 = 0x30400;
const LABELS: u32 = 0x30500;
const DIGITS: u32 = 0x30700;
const TABLE: u32 = 0x30800;
const CODE: u32 = 0x30900;

/// Words in VARS, which the Mac sets per arm before starting the program.
const FRAMES: u32 = 0;
const CAL_X: u32 = 1;
const CAL_Y: u32 = 2;
const CAL_Z: u32 = 3;
const COL_X: u32 = 4;
const COL_R: u32 = 5;

fn var(i: u32) -> u16 {
    ((VARS / 4) + i) as u16
}

const R0: u16 = 0;
const R1: u16 = 1;
const R2: u16 = 2;
const R3: u16 = 3;
const R4: u16 = 4;
const R5: u16 = 5;
const R6: u16 = 6;
const R7: u16 = 7;
const R8: u16 = 8;
const R9: u16 = 9;
const R10: u16 = 10;
const R11: u16 = 11;

const SCALE: u8 = 2;
const ADVANCE: u32 = 8 * SCALE as u32;
const ASCENT: u32 = 7 * SCALE as u32;
const ROW: u32 = ADVANCE + 2;
const COL_W: u32 = 208;
const TEXT_Y: u32 = 96;
/// The level line's box, between the two columns.
const BOX_X: u32 = 220;
const BOX_Y: u32 = 140;
const BOX_W: u32 = 200;
const BOX_H: u32 = 200;
const CX: u32 = BOX_X + BOX_W / 2;
const CY: u32 = BOX_Y + BOX_H / 2;
/// Pixels the line moves per radian of pitch, and its half length per g of gravity across the view.
const PITCH_PX: u32 = 480;
const HALF_LINE: u32 = 90;
const PERIOD_US: u32 = 40_000;
const TEXT_EVERY: u32 = 8;
const MASK: u32 = 2;

enum Value {
    Title,
    Prop(u8, u32),
    Frames,
}

/// The rows of each column: a label, and where its value comes from, with how many decimals the raw value carries.
fn rows(side: Side) -> Vec<(&'static str, Value)> {
    vec![
        (if side == Side::Left { "LEFT" } else { "RIGHT" }, Value::Title),
        ("ACC X", Value::Prop(prop::ACC_X, 0)),
        ("ACC Y", Value::Prop(prop::ACC_Y, 0)),
        ("ACC Z", Value::Prop(prop::ACC_Z, 0)),
        ("GYR X", Value::Prop(prop::GYR_X, 0)),
        ("GYR Y", Value::Prop(prop::GYR_Y, 0)),
        ("GYR Z", Value::Prop(prop::GYR_Z, 0)),
        ("PITCH", Value::Prop(prop::PITCH, 0)),
        ("ROLL", Value::Prop(prop::ROLL, 0)),
        ("LUX", Value::Prop(prop::LIGHT, 2)),
        ("BAT mV", Value::Prop(prop::BATT_MV, 0)),
        ("BAT mA", Value::Prop(prop::BATT_MA, 0)),
        ("TEMP C", Value::Prop(prop::BATT_TEMP, 1)),
        ("WORN", Value::Prop(prop::WORN, 0)),
        ("TOUCH", Value::Prop(prop::TOUCH_GESTURE, 0)),
        ("FRAME", Value::Frames),
    ]
}

/// The program, its constant pool, and the label strings (glyph indices) to put at LABELS.
fn program(side: Side) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, u32, u32), String> {
    let mut a = Asm::new(CODE, POOL);
    let mut labels = Vec::new();
    let question = LABELS + labels.len() as u32;
    labels.extend(v5::text_indices("?"));
    let (frame, stride, font) = (a.k(v5::FRAME), a.k(v5::STRIDE), a.k(v5::FONT));
    let (zero, one, ten, unknown) = (a.k(0), a.k(1), a.k(10), a.k(v5::UNKNOWN));

    // START: fire the timer at once.
    a.label("start");
    let (time, timer) = (a.k(prop::TIME_US as u32), a.k(prop::TIMER as u32));
    a.op(SYS_READ, &[R11, time]);
    a.op(SYS_WRITE, &[timer, R11]);
    a.op(END, &[]);

    // TIMER: the level line, every eighth time the text, then the timer again.
    a.label("tick");
    a.op(ADD, &[var(FRAMES), var(FRAMES), one]);
    let (bx, by, bw, bh) = (a.k(BOX_X), a.k(BOX_Y), a.k(BOX_W), a.k(BOX_H));
    a.op(FILL, &[frame, stride, bx, by, bw, bh, zero]);
    // A fixed mark in the middle to read the line against.
    let (dim, two, twenty) = (a.k(6), a.k(2), a.k(20));
    for x in [CX - 40, CX + 20] {
        let x = a.k(x);
        let y = a.k(CY);
        a.op(FILL, &[frame, stride, x, y, twenty, two, dim]);
    }
    for (reg, p) in [(R0, prop::ACC_X), (R1, prop::ACC_Y), (R2, prop::ACC_Z)] {
        let p = a.k(p as u32);
        a.op(SYS_READ, &[reg, p]);
        a.branch(BEQ, reg, unknown, "no_line");
    }
    a.op(ADD, &[R0, R0, var(CAL_X)]);
    a.op(ADD, &[R1, R1, var(CAL_Y)]);
    a.op(ADD, &[R2, R2, var(CAL_Z)]);
    // Looking up (acc_z below 0) moves the horizon down: centre y = CY + (−acc_z / 1000) · PITCH_PX.
    let (pitch_px, thousand, half, cx, cy) = (a.k(PITCH_PX), a.k(1000), a.k(HALF_LINE), a.k(CX), a.k(CY));
    a.op(MUL, &[R3, R2, pitch_px]);
    a.op(DIVS, &[R3, R3, thousand]);
    a.op(SUB, &[R3, cy, R3]);
    // The horizon runs along (acc_y, acc_x) on the panel, y down: tilting the head right turns it left.
    a.op(MUL, &[R4, R1, half]);
    a.op(DIVS, &[R4, R4, thousand]);
    a.op(MUL, &[R5, R0, half]);
    a.op(DIVS, &[R5, R5, thousand]);
    a.op(SUB, &[R6, cx, R4]);
    a.op(SUB, &[R7, R3, R5]);
    a.op(ADD, &[R8, cx, R4]);
    a.op(ADD, &[R9, R3, R5]);
    let bright = a.k(15);
    a.op(LINE, &[frame, stride, R6, R7, R8, R9, bright]);
    a.op(ADD, &[R7, R7, one]);
    a.op(ADD, &[R9, R9, one]);
    a.op(LINE, &[frame, stride, R6, R7, R8, R9, bright]);
    a.label("no_line");
    a.op(DRAW, &[frame, stride, bx, by, bx, by, bw, bh]);

    let every = a.k(TEXT_EVERY);
    a.op(MODU, &[R10, var(FRAMES), every]);
    a.branch(BNE, R10, zero, "rearm");
    let rows = rows(side);
    let (text_y, col_w, text_h) = (a.k(TEXT_Y), a.k(COL_W), a.k(ROW * rows.len() as u32));
    a.op(FILL, &[frame, stride, var(COL_X), text_y, col_w, text_h, zero]);
    let mask = a.k(MASK);
    for (i, (label, value)) in rows.iter().enumerate() {
        let baseline = a.k(TEXT_Y + i as u32 * ROW + ASCENT);
        let indices = v5::text_indices(label);
        let (at, n) = (a.k(LABELS + labels.len() as u32), a.k(indices.len() as u32));
        labels.extend(indices);
        let level = a.k(if matches!(value, Value::Title) { 15 } else { 8 });
        a.op(GLYPHS, &[font, frame, stride, var(COL_X), baseline, mask, level, at, n]);
        let decimals = match value {
            Value::Title => continue,
            Value::Prop(p, decimals) => {
                let p = a.k(*p as u32);
                a.op(SYS_READ, &[R0, p]);
                *decimals
            }
            Value::Frames => {
                a.op(MOV, &[R0, var(FRAMES)]);
                0
            }
        };
        let decimals = a.k(decimals);
        a.op(MOV, &[R1, var(COL_R)]);
        a.op(MOV, &[R2, baseline]);
        a.op(MOV, &[R9, decimals]);
        a.jump(CALL, "number");
    }
    a.op(DRAW, &[frame, stride, var(COL_X), text_y, var(COL_X), text_y, col_w, text_h]);

    a.label("rearm");
    let period = a.k(PERIOD_US);
    a.op(SYS_READ, &[R11, time]);
    a.op(ADD, &[R11, R11, period]);
    a.op(SYS_WRITE, &[timer, R11]);
    a.op(END, &[]);

    // number: r0 (signed, with r9 decimals) drawn right-aligned to x r1 on baseline r2, or `?` for UNKNOWN. Digits
    // go into DIGITS from its end backwards, as in VM.md §4.3.
    let (digits, end, dot, minus, advance) = (
        a.k(DIGITS),
        a.k(16),
        a.k(v5::text_indices(".")[0] as u32),
        a.k(v5::text_indices("-")[0] as u32),
        a.k(ADVANCE),
    );
    a.label("number");
    a.branch(BEQ, R0, unknown, "number_unknown");
    a.op(MOV, &[R3, zero]);
    a.branch(BGES, R0, zero, "number_positive");
    a.op(SUB, &[R0, zero, R0]);
    a.op(MOV, &[R3, one]);
    a.label("number_positive");
    a.op(MOV, &[R4, end]);
    a.op(MOV, &[R10, zero]);
    a.label("number_digit");
    // The decimal point goes in once `decimals` digits are out.
    a.branch(BEQ, R9, zero, "number_no_dot");
    a.branch(BNE, R10, R9, "number_no_dot");
    a.op(SUB, &[R4, R4, one]);
    a.op(ST8, &[digits, R4, dot]);
    a.label("number_no_dot");
    a.op(SUB, &[R4, R4, one]);
    a.op(MODU, &[R5, R0, ten]);
    a.op(ST8, &[digits, R4, R5]);
    a.op(DIVU, &[R0, R0, ten]);
    a.op(ADD, &[R10, R10, one]);
    a.branch(BNE, R0, zero, "number_digit");
    // At least one digit before the point: 5 with one decimal is 0.5.
    a.branch(BGES, R9, R10, "number_digit");
    a.branch(BEQ, R3, zero, "number_draw");
    a.op(SUB, &[R4, R4, one]);
    a.op(ST8, &[digits, R4, minus]);
    a.label("number_draw");
    a.op(ADD, &[R6, R4, digits]);
    a.op(SUB, &[R7, end, R4]);
    a.op(MUL, &[R8, R7, advance]);
    a.op(SUB, &[R8, R1, R8]);
    a.op(GLYPHS, &[font, frame, stride, R8, R2, mask, bright, R6, R7]);
    a.op(RET, &[]);
    a.label("number_unknown");
    a.op(SUB, &[R8, R1, advance]);
    let question = a.k(question);
    a.op(GLYPHS, &[font, frame, stride, R8, R2, mask, bright, question, one]);
    a.op(RET, &[]);

    let (start, tick) = (a.address("start"), a.address("tick"));
    let (code, pool) = a.finish()?;
    assert!(POOL + pool.len() as u32 <= VARS && LABELS + labels.len() as u32 <= DIGITS);
    Ok((code, pool, labels, start, tick))
}

/// Mean acceleration over about a second, in mg.
async fn mean_acceleration(arm: &mut Arm) -> Result<[i32; 3], Error> {
    let mut sum = [0i64; 3];
    let mut n = 0;
    for _ in 0..20 {
        let p = arm.props(&[prop::ACC_X, prop::ACC_Y, prop::ACC_Z]).await?;
        if p.iter().all(|&v| v != v5::UNKNOWN) {
            for (s, v) in sum.iter_mut().zip(p) {
                *s += v as i32 as i64;
            }
            n += 1;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    if n == 0 {
        return Err(format!("{} arm's accelerometer gives no readings", side_name(arm.side)).into());
    }
    Ok(sum.map(|s| (s / n) as i32))
}

pub async fn sensors(side: Option<Side>) -> Result<(), Error> {
    let mut arms = crate::v0::arms(side).await?;
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        crate::v0::upload_font(arm, SCALE).await?;
        arm.run([
            v5::set(prop::IMU_RATE, 104),
            v5::set(prop::GYRO_RATE, 104),
            v5::fill(0, 0, v5::WIDTH, v5::HEIGHT, 0),
            v5::draw(0, 0, v5::WIDTH, v5::HEIGHT),
        ])
        .await?;
        arm.flush().await?;
    }

    // Hold still for a second: the arms' accelerometers sit at slightly different angles, and each line gets the
    // difference to the pair's mean, so the two eyes' lines match.
    println!("measuring the arms' accelerometers; keep your head still for a second");
    let mut readings = Vec::new();
    for arm in &mut arms {
        readings.push(mean_acceleration(arm).await?);
    }
    let mean: Vec<i32> = (0..3).map(|i| readings.iter().map(|r| r[i]).sum::<i32>() / readings.len() as i32).collect();

    for (arm, reading) in arms.iter_mut().zip(&readings) {
        let (code, pool, labels, start, tick) = program(arm.side)?;
        let col_x = if arm.side == Side::Left { 4 } else { v5::WIDTH - 4 - COL_W };
        let vars =
            [0, mean[0] - reading[0], mean[1] - reading[1], mean[2] - reading[2], col_x as i32, (col_x + COL_W) as i32];
        let vars: Vec<u8> = vars.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut table = [0u32; 16];
        table[0] = start;
        table[1] = tick;
        let table: Vec<u8> = table.iter().flat_map(|v| v.to_le_bytes()).collect();
        let max = arm.max_code;
        let mut writes = v5::upload(POOL, &pool, max);
        writes.extend(v5::upload(VARS, &vars, max));
        writes.extend(v5::upload(LABELS, &labels, max));
        writes.extend(v5::upload(CODE, &code, max));
        writes.extend(v5::upload(TABLE, &table, max));
        arm.run(writes).await?;
        arm.flush().await?;
        arm.control(v5::ctrl::VM_START, &TABLE.to_le_bytes()).await?;
        println!(
            "{} arm: {} bytes of program running, correction {:+} {:+} {:+} mg",
            side_name(arm.side),
            code.len(),
            mean[0] - reading[0],
            mean[1] - reading[1],
            mean[2] - reading[2]
        );
    }

    // Watch for a moment in case a program stops on an error.
    tokio::time::sleep(Duration::from_secs(2)).await;
    for arm in &mut arms {
        let status = arm.control(v5::ctrl::VM_STATUS, &[]).await?;
        let state = ["stopped", "running", "halted"].get(status[0] as usize).copied().unwrap_or("?");
        let address = u32::from_le_bytes(status[8..12].try_into()?);
        let halt = status[15];
        let pauses = u16::from_le_bytes(status[29..31].try_into()?);
        print!(
            "{} arm: program {state}, busy {} ms per 100 ms, {pauses} pauses for running too long",
            side_name(arm.side),
            status[31]
        );
        if status[0] == 2 {
            print!(", halted at {address:#x}: {}", v5::error_name(halt));
        }
        println!();
    }
    for arm in arms {
        arm.disconnect().await;
    }
    println!("it keeps running without the Mac; `erisg1 v0 stop` stops it");
    Ok(())
}

pub async fn stop(side: Option<Side>) -> Result<(), Error> {
    let mut arms = crate::v0::arms(side).await?;
    for arm in &mut arms {
        arm.control(v5::ctrl::VM_STOP, &[]).await?;
        arm.run([v5::set(prop::IMU_RATE, 13), v5::set(prop::GYRO_RATE, 0), v5::set(prop::PANEL, 0)]).await?;
        arm.flush().await?;
    }
    for arm in arms {
        arm.disconnect().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles() {
        for side in [Side::Left, Side::Right] {
            let (code, pool, labels, start, tick) = program(side).unwrap();
            assert_eq!(start, CODE);
            assert!(tick > start && (tick as usize) < CODE as usize + code.len());
            assert!(CODE + code.len() as u32 <= v5::ARENA);
            assert!(!pool.is_empty() && !labels.is_empty());
        }
    }
}
