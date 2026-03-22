use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

const BRAILLE_BASE: u32 = 0x2800;

// Braille dot layout per cell (2 dot-cols × 4 dot-rows):
//   left col (dot-col 0):  dots 1,2,3,7 → bits 0x01,0x02,0x04,0x40
//   right col (dot-col 1): dots 4,5,6,8 → bits 0x08,0x10,0x20,0x80
const LEFT_DOTS: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
const RIGHT_DOTS: [u32; 4] = [0x08, 0x10, 0x20, 0x80];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveHandle {
    SampleStart,
    SampleEnd,
    LoopStart,
    LoopEnd,
}

pub struct WaveformHandles {
    pub sample_start: usize,
    pub sample_end: usize,
    pub loop_start: usize,
    pub loop_end: usize,
    pub active: ActiveHandle,
}

fn handle_color(kind: ActiveHandle, active: ActiveHandle) -> Color {
    if kind == active {
        Color::Yellow
    } else {
        match kind {
            ActiveHandle::SampleStart => Color::Green,
            ActiveHandle::SampleEnd => Color::Red,
            ActiveHandle::LoopStart => Color::Cyan,
            ActiveHandle::LoopEnd => Color::Magenta,
        }
    }
}

/// Render a downsampled amplitude buffer as Braille Unicode characters.
///
/// Each Braille cell covers 2 dot-columns × 4 dot-rows. The waveform is
/// centred vertically; positive amplitudes extend upward, negative downward.
/// The four edit handles are overlaid as coloured vertical bars; the active
/// handle is highlighted in yellow.
///
/// Returns exactly `height` ratatui `Line`s.
pub fn render_waveform(
    samples: &[f32],
    width: usize,
    height: usize,
    handles: &WaveformHandles,
) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return vec![Line::default(); height];
    }

    let total_dot_rows = height * 4;
    let total_dot_cols = width * 2;
    let center = total_dot_rows / 2;

    // For each dot-column compute the vertical lit range [top, bottom] inclusive.
    let lit_ranges: Vec<(usize, usize)> = if samples.is_empty() {
        vec![(center, center); total_dot_cols]
    } else {
        (0..total_dot_cols)
            .map(|dc| {
                let idx = (dc * samples.len() / total_dot_cols).min(samples.len() - 1);
                let amp = samples[idx].clamp(-1.0, 1.0);
                if amp >= 0.0 {
                    let excursion = (amp * center as f32) as usize;
                    (center.saturating_sub(excursion), center)
                } else {
                    let excursion = ((-amp) * (total_dot_rows - 1 - center) as f32) as usize;
                    (center, (center + excursion).min(total_dot_rows - 1))
                }
            })
            .collect()
    };

    // Map each handle's sample-buffer position to a terminal column.
    let sample_len = samples.len().max(1);
    let col_of = |pos: usize| (pos * width / sample_len).min(width.saturating_sub(1));

    // Build column → style map; active handle is inserted last so it wins conflicts.
    let mut handle_map: std::collections::HashMap<usize, Style> =
        std::collections::HashMap::new();

    let all_handles = [
        (handles.sample_start, ActiveHandle::SampleStart),
        (handles.sample_end, ActiveHandle::SampleEnd),
        (handles.loop_start, ActiveHandle::LoopStart),
        (handles.loop_end, ActiveHandle::LoopEnd),
    ];

    for &(pos, kind) in &all_handles {
        if kind != handles.active {
            handle_map.insert(
                col_of(pos),
                Style::default().fg(handle_color(kind, handles.active)),
            );
        }
    }
    for &(pos, kind) in &all_handles {
        if kind == handles.active {
            handle_map.insert(
                col_of(pos),
                Style::default().fg(handle_color(kind, handles.active)),
            );
        }
    }

    // Build one ratatui Line per terminal row.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    for row in 0..height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_style = Style::default();
        let mut current_text = String::new();

        for col in 0..width {
            let (ch, style) = if let Some(&hs) = handle_map.get(&col) {
                ('│', hs)
            } else {
                let dc0 = col * 2;
                let dc1 = col * 2 + 1;
                let mut bits: u32 = BRAILLE_BASE;

                for dot_row in 0..4usize {
                    let actual_row = row * 4 + dot_row;
                    let (top0, bot0) = lit_ranges[dc0];
                    if actual_row >= top0 && actual_row <= bot0 {
                        bits |= LEFT_DOTS[dot_row];
                    }
                    let (top1, bot1) = lit_ranges[dc1];
                    if actual_row >= top1 && actual_row <= bot1 {
                        bits |= RIGHT_DOTS[dot_row];
                    }
                }

                (char::from_u32(bits).unwrap_or(' '), Style::default())
            };

            if style != current_style {
                if !current_text.is_empty() {
                    spans.push(Span::styled(current_text.clone(), current_style));
                    current_text.clear();
                }
                current_style = style;
            }
            current_text.push(ch);
        }

        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style));
        }

        lines.push(Line::from(spans));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_handles() -> WaveformHandles {
        WaveformHandles {
            sample_start: 0,
            sample_end: 100,
            loop_start: 25,
            loop_end: 75,
            active: ActiveHandle::SampleStart,
        }
    }

    #[test]
    fn flat_buffer_does_not_panic_and_has_correct_line_count() {
        let samples = vec![0.0f32; 1000];
        let handles = default_handles();
        let lines = render_waveform(&samples, 80, 10, &handles);
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn empty_buffer_has_correct_line_count() {
        let handles = default_handles();
        let lines = render_waveform(&[], 80, 10, &handles);
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn zero_height_returns_empty() {
        let samples = vec![0.0f32; 100];
        let handles = default_handles();
        let lines = render_waveform(&samples, 80, 0, &handles);
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn output_line_count_equals_height_for_any_valid_input() {
        let samples: Vec<f32> = (0..500).map(|i| (i as f32 / 250.0 - 1.0)).collect();
        let handles = default_handles();
        for height in [1, 4, 10, 20] {
            let lines = render_waveform(&samples, 40, height, &handles);
            assert_eq!(lines.len(), height, "expected {height} lines");
        }
    }

    #[test]
    fn handle_at_position_zero_produces_colored_span_at_column_zero() {
        let samples = vec![0.0f32; 100];
        let handles = WaveformHandles {
            sample_start: 0,
            sample_end: 99,
            loop_start: 50,
            loop_end: 75,
            active: ActiveHandle::SampleStart,
        };
        let lines = render_waveform(&samples, 10, 4, &handles);
        assert_eq!(lines.len(), 4);
        for line in &lines {
            let first = &line.spans[0];
            assert!(
                first.style.fg.is_some(),
                "first span should be colored (handle at col 0)"
            );
            assert_eq!(first.content.chars().count(), 1);
        }
    }

    #[test]
    fn handle_at_max_position_produces_colored_span_at_last_column() {
        let samples = vec![0.0f32; 100];
        // sample_end at 99 → col = 99*10/100 = 9 (last column in width=10)
        let handles = WaveformHandles {
            sample_start: 0,
            sample_end: 99,
            loop_start: 25,
            loop_end: 75,
            active: ActiveHandle::SampleEnd,
        };
        let lines = render_waveform(&samples, 10, 4, &handles);
        for line in &lines {
            let last = line.spans.last().expect("line should have spans");
            assert!(
                last.style.fg.is_some(),
                "last span should be colored (active handle at last col)"
            );
        }
    }

    #[test]
    fn handle_at_mid_buffer_produces_vertical_bar_at_correct_column() {
        let samples = vec![0.0f32; 100];
        let width = 10;
        // loop_start=50/100 → col = 50*10/100 = 5
        let handles = WaveformHandles {
            sample_start: 0,
            sample_end: 99,
            loop_start: 50,
            loop_end: 75,
            active: ActiveHandle::LoopStart,
        };
        let lines = render_waveform(&samples, width, 4, &handles);
        let expected_col = 5;
        for line in &lines {
            let mut col = 0usize;
            let mut found = false;
            for span in &line.spans {
                if col == expected_col {
                    assert!(
                        span.style.fg.is_some(),
                        "span at col {expected_col} should be colored"
                    );
                    found = true;
                }
                col += span.content.chars().count();
            }
            assert!(found, "no span found starting at column {expected_col}");
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        let samples: Vec<f32> = (0..200).map(|i| (i as f32 / 100.0 - 1.0) * 0.8).collect();
        let handles = default_handles();
        let lines1 = render_waveform(&samples, 40, 8, &handles);
        let lines2 = render_waveform(&samples, 40, 8, &handles);
        assert_eq!(lines1.len(), lines2.len());
        for (l1, l2) in lines1.iter().zip(lines2.iter()) {
            assert_eq!(l1.spans.len(), l2.spans.len(), "span counts differ");
            for (s1, s2) in l1.spans.iter().zip(l2.spans.iter()) {
                assert_eq!(s1.content, s2.content, "span content differs");
                assert_eq!(s1.style, s2.style, "span style differs");
            }
        }
    }
}
