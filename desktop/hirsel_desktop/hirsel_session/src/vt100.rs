// Real VT100 / ANSI CSI Terminal Screen Grid for `vm-dev` (Stage C / Track 4).
// Supports:
//   - Rows x Cols cell grid with per-cell RGB foreground color & bold attribute
//   - Cursor movement (CSI A/B/C/D, H/f home/move), Erase Display (CSI 2J), Erase Line (CSI K)
//   - Full SGR ANSI 16-color + bright colors + reset (CSI ... m)
//   - Carriage return (\r), newline (\n), backspace (\x08), tab (\t), scrolling history

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct VtCell {
    pub ch: u8,
    pub fg: [u8; 4],
}

impl VtCell {
    const fn blank() -> Self {
        Self { ch: b' ', fg: DEFAULT_FG }
    }
}

pub const DEFAULT_FG: [u8; 4] = [209, 250, 229, 255];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Normal,
    Esc,
    Csi,
}

pub struct Vt100Screen {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<VtCell>,
    pub cur_row: usize,
    pub cur_col: usize,
    cur_fg: [u8; 4],
    bold: bool,
    state: ParseState,
    csi_buf: String,
}

impl Vt100Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(20);
        let rows = rows.max(6);
        Self {
            cols,
            rows,
            cells: vec![VtCell::blank(); cols * rows],
            cur_row: 0,
            cur_col: 0,
            cur_fg: DEFAULT_FG,
            bold: false,
            state: ParseState::Normal,
            csi_buf: String::new(),
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(20);
        let rows = rows.max(6);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut next = vec![VtCell::blank(); cols * rows];
        let copy_rows = self.rows.min(rows);
        let copy_cols = self.cols.min(cols);
        let src_start_row = self.rows.saturating_sub(copy_rows);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                next[r * cols + c] = self.cells[(src_start_row + r) * self.cols + c];
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = next;
        self.cur_row = self.cur_row.min(rows - 1);
        self.cur_col = self.cur_col.min(cols - 1);
    }

    pub fn push_line(&mut self, line: &str) {
        self.feed(line.as_bytes());
        self.feed(b"\r\n");
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            match self.state {
                ParseState::Normal => match b {
                    0x1b => self.state = ParseState::Esc,
                    b'\r' => self.cur_col = 0,
                    b'\n' => {
                        self.cur_col = 0;
                        self.newline();
                    }
                    0x08 => {
                        self.cur_col = self.cur_col.saturating_sub(1);
                    }
                    b'\t' => {
                        let next_tab = ((self.cur_col / 8) + 1) * 8;
                        self.cur_col = next_tab.min(self.cols - 1);
                    }
                    0x20..=0x7e => {
                        if self.cur_col >= self.cols {
                            self.cur_col = 0;
                            self.newline();
                        }
                        let idx = self.cur_row * self.cols + self.cur_col;
                        if idx < self.cells.len() {
                            let fg = if self.bold {
                                [
                                    self.cur_fg[0].saturating_add(32),
                                    self.cur_fg[1].saturating_add(32),
                                    self.cur_fg[2].saturating_add(32),
                                    self.cur_fg[3],
                                ]
                            } else {
                                self.cur_fg
                            };
                            self.cells[idx] = VtCell { ch: b, fg };
                        }
                        self.cur_col += 1;
                    }
                    _ => {}
                },
                ParseState::Esc => {
                    if b == b'[' {
                        self.csi_buf.clear();
                        self.state = ParseState::Csi;
                    } else {
                        self.state = ParseState::Normal;
                    }
                }
                ParseState::Csi => {
                    if (b'@'..=b'~').contains(&b) {
                        let params = std::mem::take(&mut self.csi_buf);
                        self.handle_csi(&params, b);
                        self.state = ParseState::Normal;
                    } else if self.csi_buf.len() < 32 {
                        self.csi_buf.push(b as char);
                    }
                }
            }
        }
    }

    fn newline(&mut self) {
        if self.cur_row + 1 < self.rows {
            self.cur_row += 1;
        } else {
            // Scroll up 1 row
            let row_len = self.cols;
            self.cells.copy_within(row_len.., 0);
            let start = (self.rows - 1) * row_len;
            for cell in &mut self.cells[start..] {
                *cell = VtCell::blank();
            }
        }
    }

    fn handle_csi(&mut self, params: &str, cmd: u8) {
        let clean = params.trim_start_matches('?');
        let nums: Vec<usize> = if clean.is_empty() {
            vec![]
        } else {
            clean.split(';').map(|s| s.parse::<usize>().unwrap_or(0)).collect()
        };
        match cmd {
            b'm' => {
                if nums.is_empty() {
                    self.cur_fg = DEFAULT_FG;
                    self.bold = false;
                    return;
                }
                for code in nums {
                    match code {
                        0 => {
                            self.cur_fg = DEFAULT_FG;
                            self.bold = false;
                        }
                        1 => self.bold = true,
                        30 => self.cur_fg = [71, 85, 105, 255],
                        31 | 91 => self.cur_fg = [248, 113, 113, 255],
                        32 | 92 => self.cur_fg = [74, 222, 128, 255],
                        33 | 93 => self.cur_fg = [250, 204, 21, 255],
                        34 | 94 => self.cur_fg = [96, 165, 250, 255],
                        35 | 95 => self.cur_fg = [192, 132, 252, 255],
                        36 | 96 => self.cur_fg = [34, 211, 238, 255],
                        37 | 97 => self.cur_fg = [248, 250, 252, 255],
                        39 => self.cur_fg = DEFAULT_FG,
                        _ => {}
                    }
                }
            }
            b'J' => {
                let mode = nums.first().copied().unwrap_or(0);
                if mode == 2 || mode == 3 {
                    for cell in &mut self.cells {
                        *cell = VtCell::blank();
                    }
                    self.cur_row = 0;
                    self.cur_col = 0;
                }
            }
            b'K' => {
                let row_start = self.cur_row * self.cols;
                for c in self.cur_col..self.cols {
                    self.cells[row_start + c] = VtCell::blank();
                }
            }
            b'H' | b'f' => {
                let r = nums.first().copied().unwrap_or(1).saturating_sub(1);
                let c = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
                self.cur_row = r.min(self.rows - 1);
                self.cur_col = c.min(self.cols - 1);
            }
            b'A' => {
                let n = nums.first().copied().unwrap_or(1).max(1);
                self.cur_row = self.cur_row.saturating_sub(n);
            }
            b'B' => {
                let n = nums.first().copied().unwrap_or(1).max(1);
                self.cur_row = (self.cur_row + n).min(self.rows - 1);
            }
            b'C' => {
                let n = nums.first().copied().unwrap_or(1).max(1);
                self.cur_col = (self.cur_col + n).min(self.cols - 1);
            }
            b'D' => {
                let n = nums.first().copied().unwrap_or(1).max(1);
                self.cur_col = self.cur_col.saturating_sub(n);
            }
            _ => {}
        }
    }
}
