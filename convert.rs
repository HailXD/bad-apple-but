use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const WIDTH: usize = 1280;
const HEIGHT: usize = 720;
const PIXELS: usize = WIDTH * HEIGHT;
const DEFAULT_FPS: usize = 30;
const DEFAULT_COUNT: usize = 1;
const FUTURE_FRAMES: usize = 30;
const SAMPLES: usize = 4096;
const TOP: usize = 16;
const MAX_BATCH: usize = 8;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const BANDS: i32 = 15;
const MAX_RADIUS: i32 = 900;
const RADII: [i32; 19] = [
    1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 360, 512, 720,
];

#[derive(Clone, Copy)]
enum Mode {
    Circle,
    Future,
}

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    mode: Option<Mode>,
    fps: usize,
    count: usize,
    grid: Option<Vec<usize>>,
    all: bool,
}

#[derive(Clone, Copy)]
struct Circle {
    x: i32,
    y: i32,
    radius: i32,
    white: bool,
    score: i64,
}

struct Prefix {
    black: Vec<i64>,
    white: Vec<i64>,
}

struct Frames {
    queue: VecDeque<Vec<u8>>,
    sum: Vec<u32>,
    linear: Vec<u32>,
    weighted: Vec<u32>,
    limit: usize,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    fn range(&mut self, start: i32, end: i32) -> i32 {
        start + (self.next() as u64 * (end - start) as u64 >> 32) as i32
    }
}

impl Frames {
    fn new(limit: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            sum: vec![0; PIXELS],
            linear: vec![0; PIXELS],
            weighted: vec![0; PIXELS],
            limit,
        }
    }

    fn fill(&mut self, input: &mut ChildStdout) -> io::Result<()> {
        while self.queue.len() < self.limit {
            let Some(frame) = read_frame(input)? else {
                break;
            };
            self.push(frame);
        }
        Ok(())
    }

    fn push(&mut self, frame: Vec<u8>) {
        for i in 0..PIXELS {
            let value = frame[i] as u32;
            self.weighted[i] += 2 * self.linear[i] + self.sum[i] + value;
            self.linear[i] += self.sum[i] + value;
            self.sum[i] += value;
        }
        self.queue.push_back(frame);
    }

    fn advance(&mut self, input: &mut ChildStdout) -> io::Result<()> {
        let count = self.queue.len() as u32;
        let frame = self.queue.pop_front().unwrap();
        for i in 0..PIXELS {
            let value = frame[i] as u32;
            self.sum[i] -= value;
            self.linear[i] -= count * value;
            self.weighted[i] -= count * count * value;
        }
        if let Some(frame) = read_frame(input)? {
            self.push(frame);
        }
        Ok(())
    }
}

fn args() -> Result<Args, String> {
    let mut values = env::args_os().skip(1).peekable();
    if values.peek().is_none() {
        return interactive_args();
    }
    let mut input = None;
    let mut output = None;
    let mut mode = None;
    let mut fps = DEFAULT_FPS;
    let mut count = DEFAULT_COUNT;
    let mut grid = None;
    let mut all = false;
    while let Some(flag) = values.next() {
        if flag == "--all" {
            all = true;
            continue;
        }
        let value = values
            .next()
            .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
        match flag.to_str() {
            Some("-i") => input = Some(value.into()),
            Some("-o") => output = Some(value.into()),
            Some("--type") => {
                mode = Some(match value.to_str() {
                    Some("circle") => Mode::Circle,
                    Some("circle-future") => Mode::Future,
                    _ => return Err("type must be circle or circle-future".into()),
                })
            }
            Some("--fps") => {
                fps = value
                    .to_str()
                    .ok_or("fps must be a positive integer")?
                    .parse::<usize>()
                    .map_err(|_| "fps must be a positive integer")?;
                if fps == 0 {
                    return Err("fps must be a positive integer".into());
                }
            }
            Some("--count") => {
                count = value
                    .to_str()
                    .ok_or("count must be a positive integer")?
                    .parse::<usize>()
                    .map_err(|_| "count must be a positive integer")?;
                if count == 0 {
                    return Err("count must be a positive integer".into());
                }
            }
            Some("--grid") => {
                grid = Some(parse_counts(
                    value
                        .to_str()
                        .ok_or("grid must contain positive integers")?,
                )?);
            }
            _ => return Err(format!("unknown option {}", flag.to_string_lossy())),
        }
    }
    if !all && output.is_none() {
        return Err("missing -o".into());
    }
    if !all && mode.is_none() {
        return Err("missing --type".into());
    }
    Ok(Args {
        input: input.ok_or("missing -i")?,
        output,
        mode,
        fps,
        count,
        grid,
        all,
    })
}

fn parse_counts(value: &str) -> Result<Vec<usize>, String> {
    let counts = value
        .split(',')
        .map(|count| count.trim().parse::<usize>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "counts must be positive integers")?;
    if counts.is_empty() || counts.contains(&0) {
        return Err("counts must be positive integers".into());
    }
    Ok(counts)
}

fn interactive_args() -> Result<Args, String> {
    const GUI: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$form = [Windows.Forms.Form]::new()
$form.Text = 'Bad Apple converter'
$form.ClientSize = [Drawing.Size]::new(540, 430)
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.TopMost = $true
$form.StartPosition = 'CenterScreen'
$form.BackColor = [Drawing.ColorTranslator]::FromHtml('#a8d')
$font = [Drawing.Font]::new('Spline Sans', 10)
$form.Font = $font
$labelColor = [Drawing.ColorTranslator]::FromHtml('#223')
$fieldColor = [Drawing.ColorTranslator]::FromHtml('#112')
$textColor = [Drawing.ColorTranslator]::FromHtml('#eee')
$accent = [Drawing.ColorTranslator]::FromHtml('#7dc')
function Add-Label($text, $x, $y) {
    $label = [Windows.Forms.Label]::new()
    $label.Text = $text
    $label.Location = [Drawing.Point]::new($x, $y)
    $label.AutoSize = $true
    $label.ForeColor = $labelColor
    [void]$form.Controls.Add($label)
}
function Set-Field($control) {
    $control.BackColor = $fieldColor
    $control.ForeColor = $textColor
}
function Add-Button($text, $x, $y, $width) {
    $button = [Windows.Forms.Button]::new()
    $button.Text = $text
    $button.Location = [Drawing.Point]::new($x, $y)
    $button.Size = [Drawing.Size]::new($width, 30)
    $button.FlatStyle = 'Flat'
    $button.BackColor = $accent
    $button.ForeColor = $labelColor
    [void]$form.Controls.Add($button)
    return $button
}
Add-Label 'Input video' 20 20
$inputPath = [Windows.Forms.TextBox]::new()
$inputPath.Location = [Drawing.Point]::new(20, 43)
$inputPath.Size = [Drawing.Size]::new(410, 28)
Set-Field $inputPath
[void]$form.Controls.Add($inputPath)
$inputButton = Add-Button 'Browse' 440 41 80
$inputButton.Add_Click({
    $dialog = [Windows.Forms.OpenFileDialog]::new()
    $dialog.Filter = 'Video files|*.mp4;*.mkv;*.webm;*.mov;*.avi|All files|*.*'
    if ($dialog.ShowDialog($form) -eq 'OK') { $inputPath.Text = $dialog.FileName }
})
Add-Label 'Type' 20 85
$type = [Windows.Forms.ComboBox]::new()
$type.Location = [Drawing.Point]::new(20, 108)
$type.Size = [Drawing.Size]::new(210, 28)
$type.DropDownStyle = 'DropDownList'
[void]$type.Items.AddRange(@('circle', 'circle-future', 'all'))
$type.SelectedIndex = 0
Set-Field $type
[void]$form.Controls.Add($type)
Add-Label 'FPS' 260 85
$fps = [Windows.Forms.NumericUpDown]::new()
$fps.Location = [Drawing.Point]::new(260, 108)
$fps.Size = [Drawing.Size]::new(120, 28)
$fps.Minimum = 1
$fps.Maximum = 240
$fps.Value = 30
$fps.BackColor = $fieldColor
$fps.ForeColor = $textColor
[void]$form.Controls.Add($fps)
Add-Label 'Counts' 20 153
$counts = [Windows.Forms.ListBox]::new()
$counts.Location = [Drawing.Point]::new(20, 176)
$counts.Size = [Drawing.Size]::new(210, 112)
Set-Field $counts
[void]$counts.Items.Add(1)
$counts.SelectedIndex = 0
[void]$form.Controls.Add($counts)
Add-Label 'Selected count' 260 153
$count = [Windows.Forms.NumericUpDown]::new()
$count.Location = [Drawing.Point]::new(260, 176)
$count.Size = [Drawing.Size]::new(120, 28)
$count.Minimum = 1
$count.Maximum = 10000
$count.Value = 1
$count.BackColor = $fieldColor
$count.ForeColor = $textColor
[void]$form.Controls.Add($count)
$set = Add-Button 'Set' 400 174 80
$add = Add-Button 'Add' 260 218 100
$remove = Add-Button 'Remove' 380 218 100
$counts.Add_SelectedIndexChanged({
    if ($counts.SelectedIndex -ge 0) { $count.Value = $counts.SelectedItem }
})
$set.Add_Click({
    if ($counts.SelectedIndex -ge 0) { $counts.Items[$counts.SelectedIndex] = [int]$count.Value }
})
$add.Add_Click({
    [void]$counts.Items.Add([int]$count.Value)
    $counts.SelectedIndex = $counts.Items.Count - 1
})
$remove.Add_Click({
    if ($counts.Items.Count -gt 1 -and $counts.SelectedIndex -ge 0) {
        $at = $counts.SelectedIndex
        $counts.Items.RemoveAt($at)
        $counts.SelectedIndex = [Math]::Min($at, $counts.Items.Count - 1)
    }
})
Add-Label 'Output file name or path (blank for automatic)' 20 305
$outputPath = [Windows.Forms.TextBox]::new()
$outputPath.Location = [Drawing.Point]::new(20, 328)
$outputPath.Size = [Drawing.Size]::new(410, 28)
Set-Field $outputPath
[void]$form.Controls.Add($outputPath)
$outputButton = Add-Button 'Browse' 440 326 80
$outputButton.Add_Click({
    $dialog = [Windows.Forms.SaveFileDialog]::new()
    $dialog.Filter = 'MP4 video|*.mp4'
    $dialog.DefaultExt = 'mp4'
    if ($dialog.ShowDialog($form) -eq 'OK') { $outputPath.Text = $dialog.FileName }
})
$type.Add_SelectedIndexChanged({
    $enabled = $type.SelectedItem -ne 'all'
    $outputPath.Enabled = $enabled
    $outputButton.Enabled = $enabled
})
$start = Add-Button 'Start' 320 380 95
$cancel = Add-Button 'Cancel' 425 380 95
$cancel.Add_Click({ $form.Close() })
$start.Add_Click({
    if (-not [IO.File]::Exists($inputPath.Text)) {
        [void][Windows.Forms.MessageBox]::Show($form, 'Select an input video', 'Bad Apple converter')
        return
    }
    $form.DialogResult = 'OK'
    $form.Close()
})
if ($form.ShowDialog() -ne 'OK') { exit 1 }
[Console]::Out.WriteLine($inputPath.Text)
[Console]::Out.WriteLine($type.SelectedItem)
[Console]::Out.WriteLine([int]$fps.Value)
[Console]::Out.WriteLine(($counts.Items -join ','))
[Console]::Out.WriteLine($outputPath.Text)
"#;
    let result = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", GUI])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("GUI: {error}"))?;
    if !result.status.success() {
        return Err("cancelled".into());
    }
    let values = String::from_utf8(result.stdout).map_err(|error| error.to_string())?;
    let mut values = values.lines();
    let input = PathBuf::from(values.next().ok_or("missing input")?);
    let mode = match values.next().ok_or("missing type")? {
        "circle" => Some(Mode::Circle),
        "circle-future" => Some(Mode::Future),
        "all" => None,
        _ => return Err("invalid type".into()),
    };
    let fps = values
        .next()
        .ok_or("missing fps")?
        .parse::<usize>()
        .map_err(|_| "invalid fps")?;
    let counts = parse_counts(values.next().ok_or("missing counts")?)?;
    let (count, grid) = if counts.len() == 1 {
        (counts[0], None)
    } else {
        (DEFAULT_COUNT, Some(counts))
    };
    let all = mode.is_none();
    let output = if all {
        None
    } else {
        let value = values.next().unwrap_or_default();
        Some(if value.is_empty() {
            output(mode.unwrap(), fps, count, grid.as_deref())
        } else {
            value.into()
        })
    };
    Ok(Args {
        input,
        output,
        mode,
        fps,
        count,
        grid,
        all,
    })
}

fn message(value: &str, error: bool) {
    let icon = if error { "Error" } else { "Information" };
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; [void][Windows.Forms.MessageBox]::Show($env:BAD_APPLE_MESSAGE, 'Bad Apple converter', 'OK', '{icon}')"
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .env("BAD_APPLE_MESSAGE", value)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

fn decoder(input: &PathBuf, fps: usize) -> io::Result<Child> {
    let filter = format!(
        "fps={fps},scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,format=gray"
    );
    Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-vf", &filter, "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdout(Stdio::piped())
        .spawn()
}

fn encoder(input: &PathBuf, output: &PathBuf, fps: usize) -> io::Result<Child> {
    let fps = fps.to_string();
    Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s:v", "1280x720", "-r", &fps,
            "-i", "-", "-i",
        ])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a?",
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-cq",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-r",
            &fps,
            "-c:a",
            "copy",
            "-shortest",
            "-y",
        ])
        .arg(output)
        .stdin(Stdio::piped())
        .spawn()
}

fn read_frame(input: &mut ChildStdout) -> io::Result<Option<Vec<u8>>> {
    let mut frame = vec![0; PIXELS];
    match input.read(&mut frame[..1])? {
        0 => Ok(None),
        _ => {
            input.read_exact(&mut frame[1..])?;
            Ok(Some(frame))
        }
    }
}

fn prefixes(canvas: &[u8], frames: &Frames, prefixes: &mut Prefix) {
    let stride = WIDTH + 1;
    let count = frames.queue.len() as i64;
    let total = 255 * count * (count + 1) * (2 * count + 1) / 6;
    for y in 0..HEIGHT {
        let mut black_row = 0;
        let mut white_row = 0;
        for x in 0..WIDTH {
            let i = y * WIDTH + x;
            let sum = 2 * frames.weighted[i] as i64;
            if canvas[i] == 0 {
                white_row += total - sum;
            } else {
                black_row += sum - total;
            }
            prefixes.black[(y + 1) * stride + x + 1] =
                prefixes.black[y * stride + x + 1] + black_row;
            prefixes.white[(y + 1) * stride + x + 1] =
                prefixes.white[y * stride + x + 1] + white_row;
        }
    }
}

fn rect_sum(prefix: &[i64], left: i32, top: i32, right: i32, bottom: i32) -> i64 {
    let left = left.clamp(0, WIDTH as i32) as usize;
    let right = right.clamp(0, WIDTH as i32) as usize;
    let top = top.clamp(0, HEIGHT as i32) as usize;
    let bottom = bottom.clamp(0, HEIGHT as i32) as usize;
    if left >= right || top >= bottom {
        return 0;
    }
    let stride = WIDTH + 1;
    prefix[bottom * stride + right] - prefix[top * stride + right] - prefix[bottom * stride + left]
        + prefix[top * stride + left]
}

fn approx(prefix: &[i64], x: i32, y: i32, radius: i32) -> i64 {
    let diameter = radius * 2 + 1;
    let mut score = 0;
    for band in 0..BANDS.min(diameter) {
        let top = -radius + band * diameter / BANDS.min(diameter);
        let bottom = -radius + (band + 1) * diameter / BANDS.min(diameter);
        let dy = (top + bottom - 1) / 2;
        let half = ((radius * radius - dy * dy) as u32).isqrt() as i32;
        score += rect_sum(prefix, x - half, y + top, x + half + 1, y + bottom);
    }
    score
}

fn exact(prefix: &[i64], x: i32, y: i32, radius: i32) -> i64 {
    (-radius..=radius)
        .map(|dy| {
            let half = ((radius * radius - dy * dy) as u32).isqrt() as i32;
            rect_sum(prefix, x - half, y + dy, x + half + 1, y + dy + 1)
        })
        .sum()
}

fn add_top(top: &mut Vec<Circle>, circle: Circle) {
    if top.len() < TOP || circle.score < top.last().unwrap().score {
        let at = top
            .binary_search_by_key(&circle.score, |item| item.score)
            .unwrap_or_else(|at| at);
        top.insert(at, circle);
        top.truncate(TOP);
    }
}

fn scored(
    prefixes: &Prefix,
    x: i32,
    y: i32,
    radius: i32,
    white: bool,
    exact_score: bool,
) -> Circle {
    let prefix = if white {
        &prefixes.white
    } else {
        &prefixes.black
    };
    let score = if exact_score {
        exact(prefix, x, y, radius)
    } else {
        approx(prefix, x, y, radius)
    };
    Circle {
        x,
        y,
        radius,
        white,
        score,
    }
}

fn intersects(x: i32, y: i32, radius: i32) -> bool {
    let dx = if x < 0 {
        -x
    } else {
        (x - WIDTH as i32 + 1).max(0)
    };
    let dy = if y < 0 {
        -y
    } else {
        (y - HEIGHT as i32 + 1).max(0)
    };
    dx * dx + dy * dy <= radius * radius
}

fn best_circle(prefixes: &Prefix, canvas: &[u8], frame: usize, samples: usize) -> Circle {
    let mut rng = Rng(0x9e3779b97f4a7c15 ^ frame as u64);
    let mut top = Vec::with_capacity(TOP + 1);
    for sample in 0..samples {
        let radius = if sample % 3 == 0 {
            RADII[(sample / 3) % RADII.len()]
        } else if sample % 3 == 1 {
            let value = rng.next() as u64;
            1 + (value as u128 * value as u128 * MAX_RADIUS as u128 / (u32::MAX as u128).pow(2))
                as i32
        } else {
            rng.range(1, MAX_RADIUS + 1)
        };
        let outside = sample % 10 == 0;
        let x = if outside {
            rng.range(-radius, WIDTH as i32 + radius + 1)
        } else {
            rng.range(0, WIDTH as i32)
        };
        let y = if outside {
            rng.range(-radius, HEIGHT as i32 + radius + 1)
        } else {
            rng.range(0, HEIGHT as i32)
        };
        if !intersects(x, y, radius) {
            continue;
        }
        add_top(&mut top, scored(prefixes, x, y, radius, false, false));
        add_top(&mut top, scored(prefixes, x, y, radius, true, false));
    }

    let mut best = scored(prefixes, 0, 0, 1, canvas[0] != 0, true);
    for circle in top {
        let circle = scored(
            prefixes,
            circle.x,
            circle.y,
            circle.radius,
            circle.white,
            true,
        );
        if circle.score < best.score {
            best = circle;
        }
    }

    let mut step = (best.radius / 2).clamp(1, 128);
    loop {
        let base = best;
        for (x, y, radius) in [
            (base.x - step, base.y, base.radius),
            (base.x + step, base.y, base.radius),
            (base.x, base.y - step, base.radius),
            (base.x, base.y + step, base.radius),
            (base.x, base.y, (base.radius - step).max(1)),
            (base.x, base.y, (base.radius + step).min(MAX_RADIUS)),
        ] {
            if !intersects(x, y, radius) {
                continue;
            }
            for white in [false, true] {
                let circle = scored(prefixes, x, y, radius, white, true);
                if circle.score < best.score {
                    best = circle;
                }
            }
        }
        if step == 1 {
            break;
        }
        step = (step / 2).max(1);
    }
    best
}

fn draw(canvas: &mut [u8], circle: Circle) {
    let color = if circle.white { 255 } else { 0 };
    for dy in -circle.radius..=circle.radius {
        let y = circle.y + dy;
        if y < 0 || y >= HEIGHT as i32 {
            continue;
        }
        let half = ((circle.radius * circle.radius - dy * dy) as u32).isqrt() as i32;
        let left = (circle.x - half).clamp(0, WIDTH as i32) as usize;
        let right = (circle.x + half + 1).clamp(0, WIDTH as i32) as usize;
        canvas[y as usize * WIDTH + left..y as usize * WIDTH + right].fill(color);
    }
}

fn tile(canvases: &[Vec<u8>]) -> Vec<u8> {
    let mut frame = vec![0; PIXELS];
    let mut columns = canvases.len().isqrt();
    if columns * columns < canvases.len() {
        columns += 1;
    }
    let rows = canvases.len().div_ceil(columns);
    for (cell, canvas) in canvases.iter().enumerate() {
        let column = cell % columns;
        let row = cell / columns;
        let left = column * WIDTH / columns;
        let right = (column + 1) * WIDTH / columns;
        let top = row * HEIGHT / rows;
        let bottom = (row + 1) * HEIGHT / rows;
        for y in top..bottom {
            let source_y = (y - top) * HEIGHT / (bottom - top);
            for x in left..right {
                let source_x = (x - left) * WIDTH / (right - left);
                frame[y * WIDTH + x] = canvas[source_y * WIDTH + source_x];
            }
        }
    }
    frame
}

fn process(
    mode: Mode,
    fps: usize,
    counts: &[usize],
    decoder: &mut Child,
    encoder: &mut Child,
) -> Result<usize, String> {
    let mut input = decoder
        .stdout
        .take()
        .ok_or("failed to open decoder output")?;
    let mut output: ChildStdin = encoder.stdin.take().ok_or("failed to open encoder input")?;
    let limit = match mode {
        Mode::Circle => 1,
        Mode::Future => FUTURE_FRAMES,
    };
    let mut frames = Frames::new(limit);
    let mut canvases = vec![vec![0; PIXELS]; counts.len()];
    let prefix_size = (WIDTH + 1) * (HEIGHT + 1);
    let mut prefix = Prefix {
        black: vec![0; prefix_size],
        white: vec![0; prefix_size],
    };
    let mut circles = Vec::with_capacity(MAX_BATCH);
    let mut frame = 0;
    frames.fill(&mut input).map_err(|error| error.to_string())?;
    while !frames.queue.is_empty() {
        for (canvas, shapes) in canvases.iter_mut().zip(counts) {
            let batch = (*shapes / MAX_BATCH).clamp(1, MAX_BATCH);
            let samples = SAMPLES / batch;
            for start in (0..*shapes).step_by(batch) {
                prefixes(canvas, &frames, &mut prefix);
                circles.clear();
                for shape in start..(start + batch).min(*shapes) {
                    circles.push(best_circle(
                        &prefix,
                        canvas,
                        *shapes * frame + shape,
                        samples,
                    ));
                }
                for circle in circles.drain(..) {
                    draw(canvas, circle);
                }
            }
        }
        let result = if canvases.len() == 1 {
            output.write_all(&canvases[0])
        } else {
            output.write_all(&tile(&canvases))
        };
        result.map_err(|error| error.to_string())?;
        frames
            .advance(&mut input)
            .map_err(|error| error.to_string())?;
        frame += 1;
        if frame % (fps * 10) == 0 {
            eprintln!("processed {} frames", frame);
        }
    }
    drop(output);
    Ok(frame)
}

fn output(mode: Mode, fps: usize, count: usize, grid: Option<&[usize]>) -> PathBuf {
    let mut name = String::from("Bad Apple");
    if matches!(mode, Mode::Future) {
        name.push_str("-future");
    }
    if fps != DEFAULT_FPS {
        name.push_str(&format!("-{fps}"));
    }
    if let Some(counts) = grid {
        name.push_str("-grid");
        for count in counts {
            name.push_str(&format!("-{count}"));
        }
    } else if count != DEFAULT_COUNT {
        name.push_str(&format!("-{count}"));
    }
    name.push_str(".mp4");
    name.into()
}

fn compile(args: &Args, output: &PathBuf, mode: Mode) -> Result<(), String> {
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| format!("create output directory: {error}"))?;
    }
    let mut decoder =
        decoder(&args.input, args.fps).map_err(|error| format!("ffmpeg decoder: {error}"))?;
    let mut encoder = match encoder(&args.input, output, args.fps) {
        Ok(child) => child,
        Err(error) => {
            let _ = decoder.kill();
            return Err(format!("ffmpeg encoder: {error}"));
        }
    };
    let result = match &args.grid {
        Some(grid) => process(mode, args.fps, grid, &mut decoder, &mut encoder),
        None => process(mode, args.fps, &[args.count], &mut decoder, &mut encoder),
    };
    if result.is_err() {
        let _ = decoder.kill();
        let _ = encoder.kill();
    }
    let decoder_status = decoder.wait().map_err(|error| error.to_string())?;
    let encoder_status = encoder.wait().map_err(|error| error.to_string())?;
    let frames = result?;
    if !decoder_status.success() {
        return Err(format!("ffmpeg decoder exited with {decoder_status}"));
    }
    if !encoder_status.success() {
        return Err(format!("ffmpeg encoder exited with {encoder_status}"));
    }
    eprintln!("wrote {} frames", frames);
    Ok(())
}

fn run() -> Result<(), String> {
    let args = args()?;
    if args.all {
        for mode in [Mode::Circle, Mode::Future] {
            compile(
                &args,
                &output(mode, args.fps, args.count, args.grid.as_deref()),
                mode,
            )?;
        }
        return Ok(());
    }
    compile(
        &args,
        args.output.as_ref().ok_or("missing -o")?,
        args.mode.ok_or("missing --type")?,
    )
}

fn main() {
    let interactive = env::args_os().len() == 1;
    match run() {
        Ok(()) if interactive => message("Conversion complete", false),
        Err(error) if interactive && error == "cancelled" => {}
        Err(error) => {
            eprintln!("error: {error}");
            if interactive {
                message(&error, true);
            } else {
                eprintln!(
                    "usage: compile.exe -i input.mp4 [-o output.mp4 --type circle|circle-future | --all] [--fps FPS] [--count COUNT | --grid COUNTS]"
                );
                std::process::exit(1);
            }
        }
        _ => {}
    }
}
