use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};
use serde_json::Value;
use similar::DiffOp;
use imara_diff::{diff, Algorithm, Sink, intern::InternedInput, sources::byte_lines};
use std::{
    fs,
    io,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
    fs::File,
    io::Write,
    io::BufWriter,
};
use memmap2::Mmap;
use rayon::prelude::*;


// --- GITHUB DARK MODE COLOR PALETTE ---
// Switched to Standard ANSI colors for maximum compatibility
const BG_CANVAS: Color = Color::Reset;       // Default Terminal Background
const FG_DEFAULT: Color = Color::Reset;      // Default Terminal Text
const LINE_NUM_FG: Color = Color::DarkGray;  // Standard DarkGray
const BORDER_COLOR: Color = Color::DarkGray; // Standard DarkGray
const HEADER_BG: Color = Color::Blue;        // Standard Blue for Header

// Delete (Red)
const BG_DEL: Color = Color::Red;            // Standard Red Background
const FG_DEL: Color = Color::White;          // White text on Red

// Insert (Green)
const BG_ADD: Color = Color::Green;          // Standard Green Background
const FG_ADD: Color = Color::Black;          // Black text on Green (High Contrast)

// Empty (For alignment)
const BG_EMPTY: Color = Color::Reset;        // Matches default bg

// --- CONSTANTS FOR OPTIMIZATION ---
const MAX_JSON_FORMAT_SIZE: u64 = 300 * 1024 * 1024; // 300 MB Limit for Pretty Print

#[derive(Parser, Debug)]
#[command(author, version, about, after_help = "
CONTROLS:
  N / P          : Jump to Next / Previous Change
  Range 1-3      : Resolve Conflict (1: Pick Left, 2: Pick Right, 3: Pick Both)
  Arrow Left     : Pick Left (File 1)
  Arrow Right    : Pick Right (File 2)
  Backspace      : Un-resolve (Reset)
  S              : Save Merged Output
  Q / Esc        : Quit
")]
struct Args {
    /// The first file (Base/Original)
    file1: PathBuf,

    /// The second file (New/Modified)
    file2: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Resolution {
    Unresolved,
    PickLeft,   // Keep File 1
    PickRight,  // Keep File 2
    PickBoth,   // Keep File 1 then File 2
}

enum AppState {
    Loading,
    Done,
    Error(String),
    Saving(String),
}

enum AppEvent {
    Log(String),
    Done(Result<(LazyDiffView, LazyDiffView, Vec<DiffOp>)>),
}




enum ContentSource {
    Mmap(Mmap),
    Memory(Vec<u8>),
}

impl std::ops::Deref for ContentSource {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        match self {
            ContentSource::Mmap(m) => m,
            ContentSource::Memory(v) => v,
        }
    }
}




struct LazyDiffView {
    content: ContentSource,
    line_offsets: Vec<usize>,
}

impl LazyDiffView {
    fn new(path: &PathBuf) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let size = metadata.len();
        
        // Strategy: 
        // 1. If > 50MB, explicit mmap, no formatting.
        // 2. If < 50MB, read carefully. If JSON, format in memory.
        
        if size > MAX_JSON_FORMAT_SIZE {
            let file = File::open(path)?;
            let mmap = unsafe { Mmap::map(&file)? };
            return Self::from_source(ContentSource::Mmap(mmap));
        }

        // Small enough to check for JSON
        // Normalize line endings
        let raw_content = fs::read_to_string(path)?.replace("\r\n", "\n");
        let content_bytes = if should_format_json(&raw_content) {
            if let Ok(val) = serde_json::from_str::<Value>(&raw_content) {
                 if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                     pretty.into_bytes()
                 } else {
                     raw_content.into_bytes()
                 }
            } else {
                raw_content.into_bytes()
            }
        } else {
            // If strictly not formatting, we could have used mmap too, but sticking to logic
            raw_content.into_bytes()
        };

        Self::from_source(ContentSource::Memory(content_bytes))
    }
    
    fn from_source(content: ContentSource) -> Result<Self> {
         // Build line offsets (start indices of lines)
         // Parallel scanning for newlines using rayon
        let offsets: Vec<usize> = content
            .par_iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i + 1)
            .collect();
            
        let mut all_offsets = vec![0];
        all_offsets.extend(offsets);
        
        Ok(Self { content, line_offsets: all_offsets })
    }

    fn get_line(&self, line_idx: usize) -> Option<&str> {
        if line_idx >= self.line_offsets.len() {
             return None;
        }
        
        let start = self.line_offsets[line_idx];
        let end = if line_idx + 1 < self.line_offsets.len() {
            self.line_offsets[line_idx + 1] - 1 // Exclude newline
        } else {
            self.content.len()
        };
        
        if start > end { // Can happen if last char is newline
            return Some("");
        }
        
        // Saturating end in case of bounds issues, though logic should prevent
        let end = end.min(self.content.len());

        std::str::from_utf8(&self.content[start..end]).ok()
    }
    
    fn len(&self) -> usize {
        self.line_offsets.len()
    }
}

fn should_format_json(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

struct DiffCell {
    line_index: Option<usize>, 
    line_number: Option<usize>,
    style: Style,
    gutter_style: Style,
}

struct App {
    state: AppState,
    // Store DiffOps instead of full rows
    diff_ops: Vec<DiffOp>, 
    // Cumulative rows for each op (to map scroll -> op)
    op_row_counts: Vec<usize>, 
    
    file1: Option<LazyDiffView>,
    file2: Option<LazyDiffView>,
    
    scroll_offset: usize,
    scroll_state: ScrollbarState,
    spinner_index: usize,
    // (File1, File2, DiffOps)
    receiver: mpsc::Receiver<AppEvent>,

    file1_name: String,
    file2_name: String,
    loading_log: String,
    
    // Merge State
    resolutions: Vec<Resolution>,
    selected_op_index: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let f1_name = args.file1.file_name().unwrap_or_default().to_string_lossy().to_string();
    let f2_name = args.file2.file_name().unwrap_or_default().to_string_lossy().to_string();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), EnterAlternateScreen)?;

    let (tx, rx) = mpsc::channel();
    // PathBuf is Clone, so we can clone it
    let f1_path = args.file1.clone();
    let f2_path = args.file2.clone();
    let tx_clone = tx.clone();

    // Heavy lifting in a separate thread
    thread::spawn(move || {
        process_side_by_side(f1_path, f2_path, tx_clone);
    });

    let mut app = App {
        state: AppState::Loading,
        diff_ops: vec![],
        op_row_counts: vec![],
        file1: None,
        file2: None,
        scroll_offset: 0,
        scroll_state: ScrollbarState::default(),
        spinner_index: 0,
        receiver: rx,
        file1_name: f1_name,
        file2_name: f2_name,
        loading_log: "Initializing...".to_string(),
        resolutions: vec![],
        selected_op_index: None,
    };

    let res = run_app(&mut stdout, &mut app).await;

    disable_raw_mode()?;
    execute!(stdout, LeaveAlternateScreen)?;
    if let Err(e) = res {
        eprintln!("Error: {:?}", e);
    }

    Ok(())
}

impl App {
    fn total_rows(&self) -> usize {
        if self.diff_ops.is_empty() { return 0; }
        let last_op = self.diff_ops.last().unwrap();
        let last_start = self.op_row_counts.last().unwrap_or(&0);
        let len = match last_op {
            DiffOp::Equal { len, .. } => *len,
            DiffOp::Delete { old_len, .. } => *old_len,
            DiffOp::Insert { new_len, .. } => *new_len,
            DiffOp::Replace { old_len, new_len, .. } => std::cmp::max(*old_len, *new_len),
        };
        last_start + len
    }
}

async fn run_app(terminal: &mut io::Stdout, app: &mut App) -> Result<()> {
    let mut t = Terminal::new(CrosstermBackend::new(terminal))?;

    loop {
        t.draw(|f| ui(f, app))?;

        if let AppState::Loading = app.state {
            app.spinner_index = app.spinner_index.wrapping_add(1);
            // Non-blocking check for the result
            while let Ok(event) = app.receiver.try_recv() {
                match event {
                    AppEvent::Log(msg) => {
                        app.loading_log = msg;
                    }
                    AppEvent::Done(result) => {
                        match result {
                            Ok((f1, f2, ops)) => {
                                app.file1 = Some(f1);
                                app.file2 = Some(f2);
                                app.diff_ops = ops;
                                
                                // Calculate cumulative row counts
                                let mut current_row = 0;
                                app.op_row_counts = Vec::with_capacity(app.diff_ops.len());
                                for op in &app.diff_ops {
                                    app.op_row_counts.push(current_row);
                                    let rows = match op {
                                        DiffOp::Equal { len, .. } => *len,
                                        DiffOp::Delete { old_len, .. } => *old_len,
                                        DiffOp::Insert { new_len, .. } => *new_len,
                                        DiffOp::Replace { old_len, new_len, .. } => std::cmp::max(*old_len, *new_len),
                                    };
                                    current_row += rows;
                                }

                                app.scroll_state = ScrollbarState::new(current_row);
                                
                                // Initialize resolutions
                                app.resolutions = vec![Resolution::Unresolved; app.diff_ops.len()];
                                app.selected_op_index = None;
                                
                                app.state = AppState::Done;
                            }
                            Err(e) => app.state = AppState::Error(e.to_string()),
                        }
                    }
                }
            }
        }

        // Poll faster for smoother spinner animation
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Check Global Keys first if needed, or matched based on state
                    match &mut app.state {
                        AppState::Saving(input) => {
                             match key.code {
                                KeyCode::Enter => {
                                    let path = input.clone();
                                    app.state = AppState::Done; // Restore state first
                                    if let Err(_e) = save_merged_output(app, &path) {
                                        
                                    }
                                }
                                KeyCode::Esc => {
                                    app.state = AppState::Done;
                                }
                                KeyCode::Backspace => {
                                    input.pop();
                                }
                                KeyCode::Char(c) => {
                                    input.push(c);
                                }
                                _ => {}
                             }
                        }
                        AppState::Done => {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                KeyCode::Char('n') => {
                                    let start_idx = if let Some(i) = app.selected_op_index { i + 1 } else { 0 };
                                    for i in start_idx..app.diff_ops.len() {
                                        if !matches!(app.diff_ops[i], DiffOp::Equal { .. }) {
                                            app.selected_op_index = Some(i);
                                            app.scroll_offset = app.op_row_counts[i];
                                            app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                            break;
                                        }
                                    }
                                }
                                KeyCode::Char('p') => {
                                    let start_idx = if let Some(i) = app.selected_op_index { i.saturating_sub(1) } else { 0 };
                                    for i in (0..=start_idx).rev() {
                                        if !matches!(app.diff_ops[i], DiffOp::Equal { .. }) {
                                            app.selected_op_index = Some(i);
                                            app.scroll_offset = app.op_row_counts[i];
                                            app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                            break;
                                        }
                                    }
                                }
                                KeyCode::Char('1') | KeyCode::Left => {
                                     if let Some(idx) = app.selected_op_index {
                                         if idx < app.resolutions.len() {
                                             app.resolutions[idx] = Resolution::PickLeft;
                                         }
                                     } else {
                                        let step = 10;
                                        app.scroll_offset = app.scroll_offset.saturating_sub(step);
                                        app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                     }
                                }
                                KeyCode::Char('2') | KeyCode::Right => {
                                     if let Some(idx) = app.selected_op_index {
                                         if idx < app.resolutions.len() {
                                             app.resolutions[idx] = Resolution::PickRight;
                                         }
                                     } else {
                                        let step = 10;
                                        app.scroll_offset = (app.scroll_offset + step).min(app.total_rows().saturating_sub(1));
                                        app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                     }
                                }
                                KeyCode::Char('3') => {
                                     if let Some(idx) = app.selected_op_index {
                                         if idx < app.resolutions.len() {
                                             app.resolutions[idx] = Resolution::PickBoth;
                                         }
                                     }
                                }
                                KeyCode::Backspace => {
                                     if let Some(idx) = app.selected_op_index {
                                         if idx < app.resolutions.len() {
                                             app.resolutions[idx] = Resolution::Unresolved;
                                         }
                                     }
                                }
                                KeyCode::Char('s') => {
                                    app.state = AppState::Saving("merged_output.json".to_string());
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if app.scroll_offset < app.total_rows().saturating_sub(1) {
                                        app.scroll_offset += 1;
                                        app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if app.scroll_offset > 0 {
                                        app.scroll_offset -= 1;
                                        app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                    }
                                }
                                KeyCode::PageDown => {
                                    let height = t.size()?.height as usize;
                                    app.scroll_offset = (app.scroll_offset + height).min(app.total_rows().saturating_sub(1));
                                    app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                }
                                KeyCode::PageUp => {
                                    let height = t.size()?.height as usize;
                                    app.scroll_offset = app.scroll_offset.saturating_sub(height);
                                    app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                }
                                KeyCode::Home => {
                                    app.scroll_offset = 0;
                                    app.scroll_state = app.scroll_state.position(0);
                                }
                                KeyCode::End => {
                                    app.scroll_offset = app.total_rows().saturating_sub(1);
                                    app.scroll_state = app.scroll_state.position(app.scroll_offset);
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();
    
    // Fill background with default terminal color (Reset)
    let main_block = Block::default().style(Style::default().bg(BG_CANVAS));
    f.render_widget(main_block, size);

    match &app.state {
        AppState::Loading => draw_loading(f, app, size),
        AppState::Error(msg) => draw_error(f, msg, size),
        AppState::Done => draw_diff_view(f, app, size),
        AppState::Saving(input) => {
            let input_clone = input.clone();
            draw_diff_view(f, app, size); // Draw background
            draw_saving_popup(f, &input_clone, size);
        }
    }
}

fn draw_saving_popup(f: &mut Frame, input: &str, area: Rect) {
    let popup_area = centered_rect(50, 5, area); // Increased height to 5
    
    // Clear the background of the popup area
    f.render_widget(ratatui::widgets::Clear, popup_area);
    
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Save As ")
        .title_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(Color::Yellow));
        
    let inner_area = block.inner(popup_area);
    f.render_widget(block, popup_area);
    
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
             Constraint::Length(1), // Input
             Constraint::Length(1), // Spacer
             Constraint::Length(1), // Hint
        ])
        .split(inner_area);

    let p = Paragraph::new(input)
        .style(Style::default().fg(Color::White));
    f.render_widget(p, chunks[0]);
    
    let hint = Paragraph::new(" [Enter]: Confirm or [Esc]: Cancel ")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, chunks[2]);
}

fn draw_diff_view(f: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header
            Constraint::Min(0),    // Content
            Constraint::Length(1), // Footer
        ])
        .split(area);

    // HEADER
    let header_style = Style::default().fg(Color::White).bg(HEADER_BG).add_modifier(Modifier::BOLD);
    let header_text = format!(" {} ◄──► {} ", app.file1_name, app.file2_name);
    f.render_widget(Paragraph::new(header_text).alignment(Alignment::Center).style(header_style), layout[0]);

    // FOOTER
    let footer_style = Style::default().fg(Color::White).bg(HEADER_BG).add_modifier(Modifier::BOLD);
    let sel_status = if let Some(idx) = app.selected_op_index {
        format!("{}", idx + 1) // 1-based index
    } else {
        "-".to_string()
    };
    
    let resolved_count = app.resolutions.iter().filter(|r| **r != Resolution::Unresolved).count();
    let total_count = app.resolutions.len();
    
    // Condense info into one line
    let help_text = format!(" [↑/↓/N/P]: Navigate | [1/2/3/←/→]: Pick | [Backspace]: Reset | [S]: Save | [Q]: Quit | Diff: {}/{} | Resolved: {}/{} ", 
        sel_status, 
        total_count,
        resolved_count,
        total_count
    );

    f.render_widget(
        Paragraph::new(help_text)
            .alignment(Alignment::Center)
            .style(footer_style),
        layout[2],
    );

    // SPLIT CONTENT
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(layout[1]);

    let view_height = layout[1].height as usize;

    
    // Draw Backgrounds
    let left_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(BORDER_COLOR))
        .style(Style::default().bg(BG_CANVAS));
    f.render_widget(left_block.clone(), chunks[0]);

    let right_block = Block::default()
        .style(Style::default().bg(BG_CANVAS));
    f.render_widget(right_block.clone(), chunks[1]);
    
    let left_area = left_block.inner(chunks[0]);
    let right_area = right_block.inner(chunks[1]);

    // --- VIRTUAL RENDERING ---
    let start_row = app.scroll_offset;
    
    // Find the operation that contains start_row
    let start_op_idx = match app.op_row_counts.binary_search(&start_row) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };

    let mut current_y = 0;
    let mut current_row_idx = start_row;
    
    // State for line numbers
    // We need to calculate the starting line numbers based on previous ops
    // This is expensive if we iterate from 0. 
    // Optimization: We could store cumulative line counts too, but for now 
    // let's just calculate it quickly or accept that line numbers might be tricky without index.
    // Wait, DiffOp gives us indices!
    // DiffOp::Equal { old_index, new_index, .. } -> these are the file indices.
    // So we don't need cumulative line counts for line numbers. We just use op indices + offset.
    
    
    // Iterate ops starting from start_op_idx

    for i in start_op_idx..app.diff_ops.len() {
        if current_y >= view_height { break; }
        
        let op = &app.diff_ops[i];
        let op_start_row = app.op_row_counts[i];
        
        let op_len = match op {
            DiffOp::Equal { len, .. } => *len,
            DiffOp::Delete { old_len, .. } => *old_len,
            DiffOp::Insert { new_len, .. } => *new_len,
            DiffOp::Replace { old_len, new_len, .. } => std::cmp::max(*old_len, *new_len),
        };
        
        // Calculate overlap with view
        let offset_in_op = current_row_idx.saturating_sub(op_start_row);
        if offset_in_op >= op_len { continue; }
        
        let rows_remaining = op_len - offset_in_op;
        let rows_to_render = rows_remaining.min(view_height - current_y);
        
        for r in 0..rows_to_render {
             let local_idx = offset_in_op + r;
             let is_selected = app.selected_op_index == Some(i);
             let resolution = app.resolutions.get(i).copied().unwrap_or(Resolution::Unresolved);
             
             let default_gutter = Style::default().fg(LINE_NUM_FG).bg(BG_CANVAS);
             let selected_gutter = Style::default().fg(Color::Yellow).bg(Color::DarkGray).add_modifier(Modifier::BOLD);
             let gutter_style = if is_selected { selected_gutter } else { default_gutter };

             let (mut left_cell, mut right_cell) = match op {
                DiffOp::Equal { old_index, new_index, .. } => (
                    DiffCell { 
                        line_index: Some(old_index + local_idx),
                        line_number: Some(old_index + local_idx + 1), 
                        style: Style::default().fg(FG_DEFAULT).bg(BG_CANVAS),
                        gutter_style
                    },
                    DiffCell { 
                        line_index: Some(new_index + local_idx),
                        line_number: Some(new_index + local_idx + 1), 
                        style: Style::default().fg(FG_DEFAULT).bg(BG_CANVAS),
                        gutter_style
                    }
                ),
                DiffOp::Delete { old_index, .. } => (
                     DiffCell { 
                        line_index: Some(old_index + local_idx),
                        line_number: Some(old_index + local_idx + 1), 
                        style: Style::default().fg(FG_DEFAULT).bg(BG_DEL),
                        gutter_style
                    },
                    DiffCell { line_index: None, line_number: None, style: Style::default().bg(BG_EMPTY), gutter_style }
                ),
                DiffOp::Insert { new_index, .. } => (
                    DiffCell { line_index: None, line_number: None, style: Style::default().bg(BG_EMPTY), gutter_style },
                    DiffCell { 
                        line_index: Some(new_index + local_idx),
                        line_number: Some(new_index + local_idx + 1), 
                        style: Style::default().fg(FG_DEFAULT).bg(BG_ADD),
                        gutter_style
                    }
                ),
                DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    let mut is_visually_equal = false;
                    if local_idx < *old_len && local_idx < *new_len {
                        if let (Some(f1), Some(f2)) = (&app.file1, &app.file2) {
                             if let (Some(l), Some(r)) = (f1.get_line(old_index + local_idx), f2.get_line(new_index + local_idx)) {
                                 if l == r { is_visually_equal = true; }
                             }
                        }
                    }

                     let left_cell = if local_idx < *old_len {
                        DiffCell { 
                            line_index: Some(old_index + local_idx),
                            line_number: Some(old_index + local_idx + 1), 
                            style: if is_visually_equal { 
                                Style::default().fg(FG_DEFAULT).bg(BG_CANVAS) 
                            } else { 
                                Style::default().fg(FG_DEFAULT).bg(BG_DEL) 
                            },
                            gutter_style
                        }
                    } else {
                        DiffCell { line_index: None, line_number: None, style: Style::default().bg(BG_EMPTY), gutter_style }
                    };
                    
                    let right_cell = if local_idx < *new_len {
                        DiffCell { 
                            line_index: Some(new_index + local_idx),
                            line_number: Some(new_index + local_idx + 1), 
                            style: if is_visually_equal { 
                                Style::default().fg(FG_DEFAULT).bg(BG_CANVAS) 
                            } else { 
                                Style::default().fg(FG_DEFAULT).bg(BG_ADD) 
                            },
                            gutter_style
                        }
                     } else {
                        DiffCell { line_index: None, line_number: None, style: Style::default().bg(BG_EMPTY), gutter_style }
                    };
                    (left_cell, right_cell)
                }
            };
            
            // Apply Resolution Styles
            match resolution {
                Resolution::PickLeft => {
                     // Left Highlight (Ensure bright), Right Dim
                     right_cell.style = right_cell.style.fg(Color::DarkGray).bg(BG_CANVAS);
                },
                Resolution::PickRight => {
                     // Right Highlight, Left Dim
                     left_cell.style = left_cell.style.fg(Color::DarkGray).bg(BG_CANVAS);
                },
                Resolution::PickBoth => {
                     // Keep default styles (both visible)
                },
                Resolution::Unresolved => {
                    // Default
                }
            }
            
            // Render Left
            let left_rect = Rect { x: left_area.x, y: left_area.y + current_y as u16, width: left_area.width, height: 1 };
            if let Some(f1) = &app.file1 {
                render_diff_line(f, &left_cell, left_rect, f1);
            }
            
            // Render Right
            let right_rect = Rect { x: right_area.x, y: right_area.y + current_y as u16, width: right_area.width, height: 1 };
             if let Some(f2) = &app.file2 {
                render_diff_line(f, &right_cell, right_rect, f2);
            }
            
            current_y += 1;
            current_row_idx += 1;
        }
    }

    f.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .style(Style::default().fg(Color::DarkGray)),
        layout[1],
        &mut app.scroll_state,
    );
}

fn render_diff_line(f: &mut Frame, cell: &DiffCell, area: Rect, source: &LazyDiffView) {
    let buf = f.buffer_mut();
    
    // 1. Fill background for the entire line
    for x in area.left()..area.right() {
        if let Some(buf_cell) = buf.cell_mut(Position::new(x, area.top())) {
            let current_bg = buf_cell.style().bg.unwrap_or(BG_CANVAS);
            let mut style = cell.style;
            if style.bg.is_none() {
                style = style.bg(current_bg);
            }
            buf_cell.set_style(style);
        }
    }

    // 2. Render Line Number
    let line_num_str = match cell.line_number {
        Some(n) => format!("{:>4} ", n),
        None => "     ".to_string(), // 4 digits + space
    };
    
    buf.set_string(
        area.x, 
        area.y, 
        &line_num_str, 
        cell.gutter_style
    );

    // 3. Render Content
    let gutter_width = 5; // 4 digits + space
    let content_x = area.x + gutter_width + 2; // +2 for " │"
    
    // Draw Separator
    buf.set_string(
        area.x + gutter_width, 
        area.y, 
        "│", 
        Style::default().fg(BORDER_COLOR).bg(BG_CANVAS)
    );

    // Draw Text
    if let Some(idx) = cell.line_index {
        if let Some(line) = source.get_line(idx) {
             let max_width = (area.width as usize).saturating_sub(7); // 5 num + 1 space + 1 separator + 1 space
             
             // Optimization: Use chars().take() to prevent panic on unicode boundaries and truncation
             let display_content: String = line.chars().take(max_width).collect();
             
             buf.set_string(
                 content_x, 
                 area.y, 
                 format!(" {}", display_content), // Add leading space
                 cell.style
             );
        }
    }
}

fn draw_loading(f: &mut Frame, app: &mut App, area: Rect) {
    const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame = SPINNER[app.spinner_index % SPINNER.len()];
    
    let text = vec![
        Line::from(vec![
            Span::styled(frame, Style::default().fg(FG_ADD).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" Analyzing files... (Large File Mode: {})", if app.file1.as_ref().map(|f| f.len() > 100000).unwrap_or(false) { "ON" } else { "AUTO" }), Style::default().fg(FG_DEFAULT)),
        ]),
        Line::from(Span::styled(format!("{}", app.loading_log), Style::default().fg(Color::DarkGray))),
    ];
    
    let p = Paragraph::new(text).alignment(Alignment::Center);
    f.render_widget(p, centered_rect(50, 10, area));
}

fn draw_error(f: &mut Frame, msg: &str, area: Rect) {
    let p = Paragraph::new(format!("ERROR: {}", msg))
        .style(Style::default().fg(FG_DEL))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(FG_DEL)));
    f.render_widget(p, centered_rect(60, 10, area));
}

fn centered_rect(w: u16, h: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - h) / 2),
            Constraint::Percentage(h),
            Constraint::Percentage((100 - h) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - w) / 2),
            Constraint::Percentage(w),
            Constraint::Percentage((100 - w) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn process_side_by_side(p1: PathBuf, p2: PathBuf, tx: Sender<AppEvent>) {
    let internal_process = || -> Result<(LazyDiffView, LazyDiffView, Vec<DiffOp>)> {
        let p1_display = p1.to_string_lossy();
        let p2_display = p2.to_string_lossy();

        let _ = tx.send(AppEvent::Log(format!("Reading {}", p1_display)));
        let f1 = LazyDiffView::new(&p1).context("Failed to read file 1")?;
        
        let _ = tx.send(AppEvent::Log(format!("Reading {}", p2_display)));
        let f2 = LazyDiffView::new(&p2).context("Failed to read file 2")?;

        let _ = tx.send(AppEvent::Log("Calculating Diff (imara-diff)...".to_string()));
        let algorithm = Algorithm::Histogram;
        
        // Intern inputs
        let input = InternedInput::new(
            byte_lines(&f1.content), 
            byte_lines(&f2.content)
        );
        
        let sink = DiffSink::new(f1.len(), f2.len());
        let ops = diff(algorithm, &input, sink);
        
        Ok((f1, f2, ops))
    };

    let res = internal_process();
    let _ = tx.send(AppEvent::Done(res));
}

struct DiffSink {
    ops: Vec<DiffOp>,
    last_old_idx: usize,
    last_new_idx: usize,
    total_old_len: usize,
    total_new_len: usize,
}

impl DiffSink {
    fn new(total_old_len: usize, total_new_len: usize) -> Self {
        Self { 
            ops: Vec::new(),
            last_old_idx: 0,
            last_new_idx: 0,
            total_old_len,
            total_new_len,
        }
    }
}

impl Sink for DiffSink {
    type Out = Vec<DiffOp>;

    fn process_change(&mut self, before: std::ops::Range<u32>, after: std::ops::Range<u32>) {
        let old_start = before.start as usize;
        let old_end = before.end as usize;
        let new_start = after.start as usize;
        let new_end = after.end as usize;

        // Detect Equal (gap between last change and this change)
        if old_start > self.last_old_idx || new_start > self.last_new_idx {
             let len = old_start - self.last_old_idx;
             // Sanity check: new_start - self.last_new_idx should also be len
             self.ops.push(DiffOp::Equal { 
                old_index: self.last_old_idx, 
                new_index: self.last_new_idx, 
                len 
            });
        }

        let old_len = old_end - old_start;
        let new_len = new_end - new_start;

        if old_len == 0 {
            // Insert
            self.ops.push(DiffOp::Insert { 
                old_index: old_start, 
                new_index: new_start, 
                new_len 
            });
        } else if new_len == 0 {
            // Delete
            self.ops.push(DiffOp::Delete { 
                old_index: old_start, 
                old_len, 
                new_index: new_start 
            });
        } else {
            // Replace
            self.ops.push(DiffOp::Replace { 
                old_index: old_start, 
                old_len, 
                new_index: new_start, 
                new_len 
            });
        }
        
        self.last_old_idx = old_end;
        self.last_new_idx = new_end;
    }

    fn finish(mut self) -> Vec<DiffOp> {
        // Handle trailing equal
        if self.last_old_idx < self.total_old_len || self.last_new_idx < self.total_new_len {
             let len = self.total_old_len - self.last_old_idx;
             self.ops.push(DiffOp::Equal { 
                old_index: self.last_old_idx, 
                new_index: self.last_new_idx, 
                len 
            });
        }
        self.ops
    }
}

fn save_merged_output(app: &App, path: &str) -> anyhow::Result<()> {
    let file = File::create(path).context("Failed to create output file")?;
    let mut writer = BufWriter::new(file);
    
    let f1 = app.file1.as_ref().context("File 1 not loaded")?;
    let f2 = app.file2.as_ref().context("File 2 not loaded")?;
    
    for (i, op) in app.diff_ops.iter().enumerate() {
        let resolution = app.resolutions.get(i).copied().unwrap_or(Resolution::Unresolved);
        
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                let start_idx = *old_index;
                let end_idx = start_idx + len;
                
                // Get byte range from line offsets
                let start_byte = f1.line_offsets[start_idx];
                let end_byte = if end_idx < f1.line_offsets.len() {
                    f1.line_offsets[end_idx]
                } else {
                    f1.content.len()
                };
                
                writer.write_all(&f1.content[start_byte..end_byte])?;
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                match resolution {
                    Resolution::PickRight | Resolution::PickBoth => {
                        let start_idx = *new_index;
                        let end_idx = start_idx + new_len;
                        
                        let start_byte = f2.line_offsets[start_idx];
                        let end_byte = if end_idx < f2.line_offsets.len() {
                            f2.line_offsets[end_idx]
                        } else {
                            f2.content.len()
                        };
                        writer.write_all(&f2.content[start_byte..end_byte])?;
                    }
                    _ => {}
                }
            }
            DiffOp::Delete { old_index, old_len, .. } => {
                match resolution {
                     Resolution::PickRight => {} // Skip (Accept Delete)
                     _ => {
                        // PickLeft (Reject Delete), Unresolved (Default Keep), PickBoth (Keep)
                        let start_idx = *old_index;
                        let end_idx = start_idx + old_len;
                        
                        let start_byte = f1.line_offsets[start_idx];
                        let end_byte = if end_idx < f1.line_offsets.len() {
                            f1.line_offsets[end_idx]
                        } else {
                            f1.content.len()
                        };
                        writer.write_all(&f1.content[start_byte..end_byte])?;
                     }
                }
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                match resolution {
                    Resolution::PickRight => {
                         let start_idx = *new_index;
                        let end_idx = start_idx + new_len;
                        
                        let start_byte = f2.line_offsets[start_idx];
                        let end_byte = if end_idx < f2.line_offsets.len() {
                            f2.line_offsets[end_idx]
                        } else {
                            f2.content.len()
                        };
                        writer.write_all(&f2.content[start_byte..end_byte])?;
                    }
                    Resolution::PickBoth => {
                        // Write File 1 chunk
                        let start_idx_1 = *old_index;
                        let end_idx_1 = start_idx_1 + old_len;
                        let start_byte_1 = f1.line_offsets[start_idx_1];
                        let end_byte_1 = if end_idx_1 < f1.line_offsets.len() {
                            f1.line_offsets[end_idx_1]
                        } else {
                            f1.content.len()
                        };
                        writer.write_all(&f1.content[start_byte_1..end_byte_1])?;
                        
                        // Write File 2 chunk
                        let start_idx_2 = *new_index;
                        let end_idx_2 = start_idx_2 + new_len;
                        let start_byte_2 = f2.line_offsets[start_idx_2];
                        let end_byte_2 = if end_idx_2 < f2.line_offsets.len() {
                            f2.line_offsets[end_idx_2]
                        } else {
                            f2.content.len()
                        };
                        writer.write_all(&f2.content[start_byte_2..end_byte_2])?;
                    }
                    _ => {
                        // PickLeft or Unresolved -> Keep File 1
                        let start_idx = *old_index;
                        let end_idx = start_idx + old_len;
                        
                        let start_byte = f1.line_offsets[start_idx];
                        let end_byte = if end_idx < f1.line_offsets.len() {
                            f1.line_offsets[end_idx]
                        } else {
                            f1.content.len()
                        };
                        writer.write_all(&f1.content[start_byte..end_byte])?;
                    }
                }
            }
        }
    }
    
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_process_side_by_side_performance() {
        // Create large dummy files
        let p1 = PathBuf::from("test_large_1.txt");
        let p2 = PathBuf::from("test_large_2.txt");
        
        {
            let mut f1 = File::create(&p1).unwrap();
            let mut f2 = File::create(&p2).unwrap();
            
            // Write 50MB of data (~1 million lines)
            for i in 0..1_000_000 {
                writeln!(f1, "Line {}", i).unwrap();
                if i % 100 != 0 { // 1% change
                     writeln!(f2, "Line {}", i).unwrap();
                } else {
                     writeln!(f2, "Modified Line {}", i).unwrap();
                }
            }
        }

        let start = Instant::now();
        let (tx, rx) = mpsc::channel();
        let p1_clone = p1.clone();
        let p2_clone = p2.clone();
        
        let _ = thread::spawn(move || {
            process_side_by_side(p1_clone, p2_clone, tx);
        });

        // Wait for result
        let mut result = None;
        while let Ok(event) = rx.recv() {
             match event {
                 AppEvent::Log(msg) => println!("{}", msg), // Print logs to stdout
                 AppEvent::Done(res) => {
                     result = Some(res);
                     break;
                 }
            }
        }
        
        // Clean up
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);

        let duration = start.elapsed();
        
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.is_ok());
        println!("Processed 1M lines in {:?}", duration);
        
        // Expect < 2 seconds for 1M lines (patience diff on mmap should be fast)
        // Adjust threshold based on environment, but 5s is safe upper bound
        assert!(duration.as_secs() < 5, "Processing took too long: {:?}", duration);
    }

    #[test]
    fn test_save_merged_output() -> Result<()> {
        let p1 = PathBuf::from("test_save_1.txt");
        let p2 = PathBuf::from("test_save_2.txt");
        let out = PathBuf::from("test_save_out.txt");

        // Use std::fs explicitly to avoid ambiguity if any
        std::fs::write(&p1, "A\nB\nC\n")?;
        std::fs::write(&p2, "A\nMOD\nC\nD\n")?;

        let f1 = LazyDiffView::new(&p1)?;
        let f2 = LazyDiffView::new(&p2)?;

        // Manual Diff Construction matching the files
        // 1. Equal "A\n" (OLD: 0, NEW: 0, LEN: 1)
        // 2. Replace "B\n" (OLD: 1, LEN: 1) -> "MOD\n" (NEW: 1, LEN: 1)
        // 3. Equal "C\n" (OLD: 2, NEW: 2, LEN: 1)
        // 4. Insert "D\n" (NEW: 3, LEN: 1)
        
        let diff_ops = vec![
            DiffOp::Equal { old_index: 0, new_index: 0, len: 1 },
            DiffOp::Replace { old_index: 1, old_len: 1, new_index: 1, new_len: 1 },
            DiffOp::Equal { old_index: 2, new_index: 2, len: 1 },
            DiffOp::Insert { old_index: 3, new_index: 3, new_len: 1 }, // old_index for insert is point of insertion
        ];

        let mut app = App {
            state: AppState::Done,
            diff_ops: diff_ops.clone(),
            op_row_counts: vec![], // Not needed for save
            file1: Some(f1),
            file2: Some(f2),
            scroll_offset: 0,
            scroll_state: ScrollbarState::default(),
            spinner_index: 0,
            receiver: std::sync::mpsc::channel().1,
            file1_name: "f1".to_string(),
            file2_name: "f2".to_string(),
            loading_log: String::new(),
            resolutions: vec![Resolution::Unresolved; 4],
            selected_op_index: None,
        };

        // Case 1: All Unresolved -> Should match File 1 (Project "Our" changes)
        // Except Insert: File 1 has nothing, so Unresolved -> Skip
        // Delete: File 1 has content, so Unresolved -> Keep
        // Replace: File 1 has content, so Unresolved -> Keep
        // Output: "A\nB\nC\n"
        save_merged_output(&app, out.to_str().unwrap())?;
        let saved = std::fs::read_to_string(&out)?;
        assert_eq!(saved, "A\nB\nC\n");

        // Case 2: Accept Replace (Quick Right), Reject Insert (Unresolved/Left)
        // Output: "A\nMOD\nC\n"
        app.resolutions[1] = Resolution::PickRight;
        save_merged_output(&app, out.to_str().unwrap())?;
        let saved = std::fs::read_to_string(&out)?;
        assert_eq!(saved, "A\nMOD\nC\n");

        // Case 3: PickBoth for Replace ("B\nMOD\n"), PickRight for Insert ("D\n")
        // Output: "A\nB\nMOD\nC\nD\n" (Wait, existing logic writes File 1 then File 2 for Replace)
        // My manual diff construction: Replace "B\n" with "MOD\n". PickBoth -> "B\nMOD\n".
        app.resolutions[1] = Resolution::PickBoth;
        app.resolutions[3] = Resolution::PickRight; // Insert D
        save_merged_output(&app, out.to_str().unwrap())?;
        let saved = std::fs::read_to_string(&out)?;
        assert_eq!(saved, "A\nB\nMOD\nC\nD\n");

        // Cleanup
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
        let _ = std::fs::remove_file(out);
        
        Ok(())
    }

    #[test]
    fn test_interactive_session() {
         // 1. Setup App with conflicts
        let diff_ops = vec![
            DiffOp::Equal { old_index: 0, new_index: 0, len: 1 }, // Equal
            DiffOp::Replace { old_index: 1, old_len: 1, new_index: 1, new_len: 1 }, // Conflict 1
            DiffOp::Equal { old_index: 2, new_index: 2, len: 1 }, // Equal
        ];
        
        let op_row_counts = vec![0, 1, 2];

        let mut app = App {
            state: AppState::Done,
            diff_ops: diff_ops.clone(),
            op_row_counts,
            file1: None, // Not needed for logic test
            file2: None,
            scroll_offset: 0,
            scroll_state: ScrollbarState::default(),
            spinner_index: 0,
            receiver: std::sync::mpsc::channel().1,
            file1_name: "f1".to_string(),
            file2_name: "f2".to_string(),
            loading_log: String::new(),
            resolutions: vec![Resolution::Unresolved; 3],
            selected_op_index: None,
        };

        // 2. Simulate 'n' (Next Hunk) from None
        {
            let start_idx = app.selected_op_index.map(|i| i + 1).unwrap_or(0);
            for i in start_idx..app.diff_ops.len() {
                if !matches!(app.diff_ops[i], DiffOp::Equal { .. }) {
                    app.selected_op_index = Some(i);
                    // Mock scroll update
                    app.scroll_offset = app.op_row_counts[i];
                    break;
                }
            }
        }
        
        // Assert: Should find index 1 (Replace)
        assert_eq!(app.selected_op_index, Some(1));
        assert_eq!(app.scroll_offset, 1);

        // 3. Simulate '1' (PickLeft)
         if let (AppState::Done, Some(idx)) = (&app.state, app.selected_op_index) {
             if idx < app.resolutions.len() {
                 app.resolutions[idx] = Resolution::PickLeft;
             }
         }

        // Assert: Resolution at 1 should be PickLeft
        assert_eq!(app.resolutions[1], Resolution::PickLeft);
        
        // 4. Simulate 'n' again -> Should not find new conflict
        {
            let start_idx = app.selected_op_index.map(|i| i + 1).unwrap_or(0);
            for i in start_idx..app.diff_ops.len() {
                 if !matches!(app.diff_ops[i], DiffOp::Equal { .. }) {
                    app.selected_op_index = Some(i);
                    break;
                }
            }
        }
        assert_eq!(app.selected_op_index, Some(1)); // Remained 1
    }

    #[test]
    fn test_save_prompt_flow() -> Result<()> {
        let diff_ops = vec![DiffOp::Equal { old_index: 0, new_index: 0, len: 1 }];
        
        // Setup dummy file
        let path = PathBuf::from("test_prompt_1.txt");
        std::fs::write(&path, "A").unwrap();
        let f1 = LazyDiffView::new(&path).unwrap();

        // Setup dummy file 2
        let path2 = PathBuf::from("test_prompt_2.txt");
        std::fs::write(&path2, "B").unwrap();
        let f2 = LazyDiffView::new(&path2).unwrap();

        let mut app = App {
            state: AppState::Done,
            diff_ops,
            op_row_counts: vec![0],
            file1: Some(f1),
            file2: Some(f2),
            scroll_offset: 0,
            scroll_state: ScrollbarState::default(),
            spinner_index: 0,
            receiver: std::sync::mpsc::channel().1,
            file1_name: "f1".to_string(),
            file2_name: "f2".to_string(),
            loading_log: String::new(),
            resolutions: vec![Resolution::Unresolved],
            selected_op_index: None,
        };

        // 1. Initial State
        assert!(matches!(app.state, AppState::Done));

        // 2. Simulate User Input State Transition
        // (In real app, 's' triggers this)
        app.state = AppState::Saving("merged_output.json".to_string());
        
        if let AppState::Saving(input) = &app.state {
            assert_eq!(input, "merged_output.json");
        } else {
            panic!("State should be Saving");
        }

        // 3. Verify saving works with a custom path
        let custom_path = "test_custom_save.json";
        // Ensure file doesn't exist
        if std::path::Path::new(custom_path).exists() {
            std::fs::remove_file(custom_path)?;
        }
        
        // This simulates the action taken when Enter is pressed
        save_merged_output(&app, custom_path)?;
        
        assert!(std::path::Path::new(custom_path).exists());
        
        // Cleanup
        let _ = std::fs::remove_file(custom_path);
        let _ = std::fs::remove_file(path);
        
        Ok(())
    }
}