//! ASCII art frames for Eve banner animation.
//!
//! Each frame is a function returning `Vec<String>` with embedded ANSI truecolor
//! escape codes. The frames differ subtly in the torso/hip area to create a
//! sway animation effect.
//!
//! Eve is depicted as a hooded anime girl with a cable/tail accent, standing
//! with a confident posture. The hood is rendered with block/shade characters
//! for depth, the face uses lighter tones, and the cable is in amber.

use crate::colors::{EVE_AMBER, EVE_DARK, EVE_LAVENDER, EVE_LILAC, EVE_PURPLE};

// ── Color helpers ────────────────────────────────────────────────────────

fn fg(rgb: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
}

const RESET: &str = "\x1b[0m";

// ── Full-size frames (~35 lines, ~75 cols) ──────────────────────────────
//
// The character: a hooded anime-style girl seen from the front.
//   - Top: curved cable/tail in amber swooping down to her hood
//   - Hood: deep purple/dark shading with pointed edges
//   - Face: lavender/lilac with anime eyes (large, expressive)
//   - Body: a fitted cloak/robe with fold details
//   - Lower: legs/boots beneath the cloak hem
//
// The hip sway is achieved by shifting the torso section (lines ~20-30)
// left or right by 1-2 characters between frames while keeping the head
// and feet anchored.

/// Full-size Eve -- frame 1 (neutral stance).
pub fn full_frame_1() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("                                                            "),
        format!("{am}                             _,-~-._                      {r}"),
        format!("{am}                          _-'        `-._                 {r}"),
        format!("{am}                        /'      ~        `\\               {r}"),
        format!("{am}                       |                   `.             {r}"),
        format!("{am}                        `.    _              |            {r}"),
        format!("{am}                          `-,' `-.           |            {r}"),
        format!("{am}                                 `-._____.--'             {r}"),
        format!("{am}                                       |                  {r}"),
        format!("{dk}                         ╭─────────────┴──╮              {r}"),
        format!("{pr}                       ╭─┤  ░░░░░░░░░░░░  ├──╮           {r}"),
        format!("{pr}                     ╭─┤ │ ░░░▓▓▓▓▓▓▓░░░ │  ├─╮         {r}"),
        format!("{dk}                    ╭┤ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ├╮        {r}"),
        format!("{dk}                    ││ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ││        {r}"),
        format!("{pr}                    ││ ╰ │░░▓▓▓▓▓▓▓▓▓░░░ │  ╯ ││        {r}"),
        format!("{li}                    │╰   │  ░░░░░░░░░░░  │    ╯│        {r}"),
        format!("{lv}                    │    │   {li}o{dk}.:{lv}    {li}o{dk}.:{lv}   │     │        {r}"),
        format!("{lv}                    │    │      {dk}`{li},,{dk}'{lv}      │     │        {r}"),
        format!("{lv}                    │    │      {dk}\\__/{lv}       │     │        {r}"),
        format!("{pr}                    │    ╰────────────────╯     │        {r}"),
        format!("{pr}                    │       ╭────────────╮      │        {r}"),
        format!("{dk}                    │      ╭┤ ░▓▓▓▓▓▓▓░ ├╮     │        {r}"),
        format!("{dk}                    │      │╰────────────╯│     │        {r}"),
        format!("{pr}                    │      │  ╭────────╮  │     │        {r}"),
        format!("{pr}                    │      │  │ ░░░░░░ │  │     │        {r}"),
        format!("{lv}                     \\     │  │ ░░░░░░ │  │    /         {r}"),
        format!("{lv}                      \\    │  │ ░░░░░░ │  │   /          {r}"),
        format!("{pr}                       \\   ╰──┤        ├──╯  /           {r}"),
        format!("{dk}                        ╰─────┤        ├─────╯           {r}"),
        format!("{pr}                              ╭┴────────┴╮               {r}"),
        format!("{dk}                             ╭┤ ░░░░░░░░ ├╮              {r}"),
        format!("{dk}                             │╰────┬─────╯│              {r}"),
        format!("{dk}                            ╭┴─────┴──────┴╮             {r}"),
        format!("{pr}                            ╰───────────────╯             {r}"),
    ]
}

/// Full-size Eve -- frame 2 (hips shifted right).
pub fn full_frame_2() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("                                                            "),
        format!("{am}                             _,-~-._                      {r}"),
        format!("{am}                          _-'        `-._                 {r}"),
        format!("{am}                        /'      ~        `\\               {r}"),
        format!("{am}                       |                   `.             {r}"),
        format!("{am}                        `.    _              |            {r}"),
        format!("{am}                          `-,' `-.           |            {r}"),
        format!("{am}                                 `-._____.--'             {r}"),
        format!("{am}                                       |                  {r}"),
        format!("{dk}                         ╭─────────────┴──╮              {r}"),
        format!("{pr}                       ╭─┤  ░░░░░░░░░░░░  ├──╮           {r}"),
        format!("{pr}                     ╭─┤ │ ░░░▓▓▓▓▓▓▓░░░ │  ├─╮         {r}"),
        format!("{dk}                    ╭┤ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ├╮        {r}"),
        format!("{dk}                    ││ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ││        {r}"),
        format!("{pr}                    ││ ╰ │░░▓▓▓▓▓▓▓▓▓░░░ │  ╯ ││        {r}"),
        format!("{li}                    │╰   │  ░░░░░░░░░░░  │    ╯│        {r}"),
        format!("{lv}                    │    │   {li}o{dk}.:{lv}    {li}o{dk}.:{lv}   │     │        {r}"),
        format!("{lv}                    │    │      {dk}`{li},,{dk}'{lv}      │     │        {r}"),
        format!("{lv}                    │    │      {dk}\\__/{lv}       │     │        {r}"),
        format!("{pr}                    │    ╰────────────────╯     │        {r}"),
        format!("{pr}                    │        ╭────────────╮     │        {r}"),
        format!("{dk}                    │       ╭┤ ░▓▓▓▓▓▓▓░ ├╮    │        {r}"),
        format!("{dk}                    │       │╰────────────╯│    │        {r}"),
        format!("{pr}                    │       │  ╭────────╮  │    │        {r}"),
        format!("{pr}                    │       │  │ ░░░░░░ │  │    │        {r}"),
        format!("{lv}                     \\      │  │ ░░░░░░ │  │   /         {r}"),
        format!("{lv}                      \\     │  │ ░░░░░░ │  │  /          {r}"),
        format!("{pr}                       \\    ╰──┤        ├──╯ /           {r}"),
        format!("{dk}                        ╰─────┤        ├─────╯           {r}"),
        format!("{pr}                               ╭┴──────┴╮                {r}"),
        format!("{dk}                              ╭┤ ░░░░░░ ├╮               {r}"),
        format!("{dk}                              │╰───┬────╯│               {r}"),
        format!("{dk}                             ╭┴────┴─────┴╮              {r}"),
        format!("{pr}                             ╰─────────────╯              {r}"),
    ]
}

/// Full-size Eve -- frame 3 (hips shifted left).
pub fn full_frame_3() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("                                                            "),
        format!("{am}                             _,-~-._                      {r}"),
        format!("{am}                          _-'        `-._                 {r}"),
        format!("{am}                        /'      ~        `\\               {r}"),
        format!("{am}                       |                   `.             {r}"),
        format!("{am}                        `.    _              |            {r}"),
        format!("{am}                          `-,' `-.           |            {r}"),
        format!("{am}                                 `-._____.--'             {r}"),
        format!("{am}                                       |                  {r}"),
        format!("{dk}                         ╭─────────────┴──╮              {r}"),
        format!("{pr}                       ╭─┤  ░░░░░░░░░░░░  ├──╮           {r}"),
        format!("{pr}                     ╭─┤ │ ░░░▓▓▓▓▓▓▓░░░ │  ├─╮         {r}"),
        format!("{dk}                    ╭┤ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ├╮        {r}"),
        format!("{dk}                    ││ │ │░▓▓▓▓▓▓▓▓▓▓▓▓░ │  │ ││        {r}"),
        format!("{pr}                    ││ ╰ │░░▓▓▓▓▓▓▓▓▓░░░ │  ╯ ││        {r}"),
        format!("{li}                    │╰   │  ░░░░░░░░░░░  │    ╯│        {r}"),
        format!("{lv}                    │    │   {li}o{dk}.:{lv}    {li}o{dk}.:{lv}   │     │        {r}"),
        format!("{lv}                    │    │      {dk}`{li},,{dk}'{lv}      │     │        {r}"),
        format!("{lv}                    │    │      {dk}\\__/{lv}       │     │        {r}"),
        format!("{pr}                    │    ╰────────────────╯     │        {r}"),
        format!("{pr}                    │      ╭────────────╮       │        {r}"),
        format!("{dk}                    │     ╭┤ ░▓▓▓▓▓▓▓░ ├╮      │        {r}"),
        format!("{dk}                    │     │╰────────────╯│      │        {r}"),
        format!("{pr}                    │     │  ╭────────╮  │      │        {r}"),
        format!("{pr}                    │     │  │ ░░░░░░ │  │      │        {r}"),
        format!("{lv}                     \\    │  │ ░░░░░░ │  │     /         {r}"),
        format!("{lv}                      \\   │  │ ░░░░░░ │  │    /          {r}"),
        format!("{pr}                       \\  ╰──┤        ├──╯   /           {r}"),
        format!("{dk}                        ╰─────┤        ├─────╯           {r}"),
        format!("{pr}                             ╭┴────────┴╮                {r}"),
        format!("{dk}                            ╭┤ ░░░░░░░░ ├╮               {r}"),
        format!("{dk}                            │╰─────┬────╯│               {r}"),
        format!("{dk}                           ╭┴──────┴─────┴╮              {r}"),
        format!("{pr}                           ╰────────────────╯             {r}"),
    ]
}

/// Full-size Eve -- frame 4 (return to neutral).
pub fn full_frame_4() -> Vec<String> {
    full_frame_1()
}

// ── Compact frames (~18 lines, ~50 cols) ────────────────────────────────

/// Compact Eve -- frame 1 (neutral).
pub fn compact_frame_1() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("{am}                _,-~-._                {r}"),
        format!("{am}              ,' ~ _   `-.             {r}"),
        format!("{am}              `._ / `-.__/             {r}"),
        format!("{am}                  |                    {r}"),
        format!("{dk}            ╭─────┴─────╮             {r}"),
        format!("{pr}           ╭┤ ░░▓▓▓▓▓░░ ├╮            {r}"),
        format!("{dk}           ││░▓▓▓▓▓▓▓▓▓░││            {r}"),
        format!("{li}           │╰ ░{lv} {li}o{dk}:{lv}  {li}o{dk}:{lv} {li}░ ╯│            {r}"),
        format!("{lv}           │    {dk}`,,'{lv}     │            {r}"),
        format!("{lv}           │    {dk}\\__/{lv}     │            {r}"),
        format!("{pr}           ╰─────────────╯            {r}"),
        format!("{pr}            ╭───────────╮             {r}"),
        format!("{dk}            │ ░▓▓▓▓▓▓▓░ │             {r}"),
        format!("{pr}            │ ╭───────╮ │             {r}"),
        format!("{lv}             \\│ ░░░░░ │/              {r}"),
        format!("{dk}              ╰──┬─┬──╯               {r}"),
        format!("{dk}             ╭───┴─┴───╮              {r}"),
        format!("{pr}             ╰─────────╯              {r}"),
    ]
}

/// Compact Eve -- frame 2 (hips shifted right).
pub fn compact_frame_2() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("{am}                _,-~-._                {r}"),
        format!("{am}              ,' ~ _   `-.             {r}"),
        format!("{am}              `._ / `-.__/             {r}"),
        format!("{am}                  |                    {r}"),
        format!("{dk}            ╭─────┴─────╮             {r}"),
        format!("{pr}           ╭┤ ░░▓▓▓▓▓░░ ├╮            {r}"),
        format!("{dk}           ││░▓▓▓▓▓▓▓▓▓░││            {r}"),
        format!("{li}           │╰ ░{lv} {li}o{dk}:{lv}  {li}o{dk}:{lv} {li}░ ╯│            {r}"),
        format!("{lv}           │    {dk}`,,'{lv}     │            {r}"),
        format!("{lv}           │    {dk}\\__/{lv}     │            {r}"),
        format!("{pr}           ╰─────────────╯            {r}"),
        format!("{pr}             ╭───────────╮            {r}"),
        format!("{dk}             │ ░▓▓▓▓▓▓▓░ │            {r}"),
        format!("{pr}             │ ╭───────╮ │            {r}"),
        format!("{lv}              \\│ ░░░░░ │/             {r}"),
        format!("{dk}               ╰──┬─┬──╯              {r}"),
        format!("{dk}              ╭───┴─┴───╮             {r}"),
        format!("{pr}              ╰─────────╯             {r}"),
    ]
}

/// Compact Eve -- frame 3 (hips shifted left).
pub fn compact_frame_3() -> Vec<String> {
    let dk = fg(EVE_DARK);
    let lv = fg(EVE_LAVENDER);
    let pr = fg(EVE_PURPLE);
    let li = fg(EVE_LILAC);
    let am = fg(EVE_AMBER);
    let r = RESET;

    vec![
        format!("{am}                _,-~-._                {r}"),
        format!("{am}              ,' ~ _   `-.             {r}"),
        format!("{am}              `._ / `-.__/             {r}"),
        format!("{am}                  |                    {r}"),
        format!("{dk}            ╭─────┴─────╮             {r}"),
        format!("{pr}           ╭┤ ░░▓▓▓▓▓░░ ├╮            {r}"),
        format!("{dk}           ││░▓▓▓▓▓▓▓▓▓░││            {r}"),
        format!("{li}           │╰ ░{lv} {li}o{dk}:{lv}  {li}o{dk}:{lv} {li}░ ╯│            {r}"),
        format!("{lv}           │    {dk}`,,'{lv}     │            {r}"),
        format!("{lv}           │    {dk}\\__/{lv}     │            {r}"),
        format!("{pr}           ╰─────────────╯            {r}"),
        format!("{pr}           ╭───────────╮              {r}"),
        format!("{dk}           │ ░▓▓▓▓▓▓▓░ │              {r}"),
        format!("{pr}           │ ╭───────╮ │              {r}"),
        format!("{lv}            \\│ ░░░░░ │/               {r}"),
        format!("{dk}             ╰──┬─┬──╯                {r}"),
        format!("{dk}            ╭───┴─┴───╮               {r}"),
        format!("{pr}            ╰─────────╯               {r}"),
    ]
}

/// Compact Eve -- frame 4 (return to neutral).
pub fn compact_frame_4() -> Vec<String> {
    compact_frame_1()
}

// ── Frame accessors ─────────────────────────────────────────────────────

/// Returns the 4 full-size animation frames.
pub fn full_frames() -> [Vec<String>; 4] {
    [full_frame_1(), full_frame_2(), full_frame_3(), full_frame_4()]
}

/// Returns the 4 compact animation frames.
pub fn compact_frames() -> [Vec<String>; 4] {
    [
        compact_frame_1(),
        compact_frame_2(),
        compact_frame_3(),
        compact_frame_4(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_frames_all_same_height() {
        let frames = full_frames();
        let height = frames[0].len();
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(
                frame.len(),
                height,
                "full frame {i} has {} lines, expected {height}",
                frame.len()
            );
        }
    }

    #[test]
    fn compact_frames_all_same_height() {
        let frames = compact_frames();
        let height = frames[0].len();
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(
                frame.len(),
                height,
                "compact frame {i} has {} lines, expected {height}",
                frame.len()
            );
        }
    }

    #[test]
    fn full_frame_height_in_range() {
        let frame = full_frame_1();
        assert!(
            frame.len() >= 25 && frame.len() <= 45,
            "full frame height {} out of expected range 25..45",
            frame.len()
        );
    }

    #[test]
    fn compact_frame_height_in_range() {
        let frame = compact_frame_1();
        assert!(
            frame.len() >= 12 && frame.len() <= 25,
            "compact frame height {} out of expected range 12..25",
            frame.len()
        );
    }
}
