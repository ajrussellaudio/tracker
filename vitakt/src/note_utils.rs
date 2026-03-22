pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

// ── Phrase-editor column indices ─────────────────────────────────────────────
pub const COL_NOTE: usize = 0;
pub const COL_INS: usize = 1;
/// Columns 2–9: FX slot pairs (cmd at even offsets, val at odd offsets).
/// `col_to_fx(col)` returns `Some((slot_index, is_cmd_field))` for FX columns.
pub const COL_FX_FIRST: usize = 2;
pub const COL_COUNT: usize = 10; // note + ins + 4×(cmd+val)

pub fn note_name(midi: u8) -> String {
    let octave = (midi as i32 / 12) - 1;
    let name = NOTE_NAMES[(midi % 12) as usize];
    if name.len() == 1 {
        format!("{name}-{octave}")
    } else {
        format!("{name}{octave}")
    }
}

/// Map a QWERTY key to a semitone offset from C (standard 2-octave tracker layout).
/// Lower row: z=C(0) s=C#(1) x=D(2) d=D#(3) c=E(4) v=F(5) g=F#(6) b=G(7) h=G#(8) n=A(9) j=A#(10) m=B(11)
/// Upper row: q=C(12) 2=C#(13) w=D(14) 3=D#(15) e=E(16) r=F(17) 5=F#(18) t=G(19) 6=G#(20) y=A(21) 7=A#(22) u=B(23)
pub fn qwerty_to_semitone(c: char) -> Option<i8> {
    match c {
        'z' => Some(0),
        's' => Some(1),
        'x' => Some(2),
        'd' => Some(3),
        'c' => Some(4),
        'v' => Some(5),
        'g' => Some(6),
        'b' => Some(7),
        'h' => Some(8),
        'n' => Some(9),
        'j' => Some(10),
        'm' => Some(11),
        'q' => Some(12),
        '2' => Some(13),
        'w' => Some(14),
        '3' => Some(15),
        'e' => Some(16),
        'r' => Some(17),
        '5' => Some(18),
        't' => Some(19),
        '6' => Some(20),
        'y' => Some(21),
        '7' => Some(22),
        'u' => Some(23),
        _ => None,
    }
}

/// Compute playback speed ratio from note vs root using equal temperament.
pub fn pitch_speed(note: u8, root_note: u8) -> f32 {
    let delta = note as i32 - root_note as i32;
    2.0_f64.powf(delta as f64 / 12.0) as f32
}

pub fn col_to_fx(col: usize) -> Option<(usize, bool)> {
    if col >= COL_FX_FIRST && col < COL_COUNT {
        let offset = col - COL_FX_FIRST;
        Some((offset / 2, offset % 2 == 0)) // (slot_index, is_cmd)
    } else {
        None
    }
}
