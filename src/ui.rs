use eframe::egui;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use duplicate_searcher::{
    classify, extension_of, format_bytes, format_count, format_mtime, inventory, load_preview,
    search, summarize, DupFile, DupGroup, FormatInfo, FormatKind, Inventory, Preview, Progress,
    Sample, ScanOptions, SearchOptions, SearchOutcome,
};

const FULL_HASH_BELOW: u64 = 1024 * 1024;
const MIN_SIZES: &[(u64, &str)] = &[
    (1, "от 1 Б"),
    (100 * 1024, "от 100 КиБ"),
    (1024 * 1024, "от 1 МиБ"),
    (10 * 1024 * 1024, "от 10 МиБ"),
    (100 * 1024 * 1024, "от 100 МиБ"),
    (1024 * 1024 * 1024, "от 1 ГиБ"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Formats,
    Duplicates,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Inventory,
    Search,
}

enum JobResult {
    Inventory(Result<Inventory, String>),
    Search(Result<SearchOutcome, String>),
}

struct Job {
    kind: JobKind,
    progress: Arc<Progress>,
    rx: Receiver<JobResult>,
}

#[derive(Clone)]
struct Focus {
    path: PathBuf,
    size: u64,
    modified: Option<i64>,
    ext: String,
    kind: FormatKind,
}

enum Cached {
    Image(egui::TextureHandle),
    Text(String),
    Unavailable(String),
}

#[derive(Default)]
struct CardActions {
    toggles: Vec<(String, bool)>,
    focus: Option<Focus>,
    want: Vec<(PathBuf, FormatKind)>,
}

pub struct DuplicateApp {
    root: Option<PathBuf>,
    min_size: u64,
    by_content: bool,
    by_size: bool,
    by_name: bool,
    by_mtime: bool,
    skip_empty: bool,
    collapse_hardlinks: bool,
    skip_junk: bool,
    one_fs: bool,
    follow_symlinks: bool,
    options_dirty: bool,
    filter: String,
    dup_filter: String,
    tab: Tab,
    inventory: Option<Inventory>,
    formats: Vec<FormatInfo>,
    selected: HashSet<String>,
    stats_dirty: bool,
    selected_files: u64,
    selected_bytes: u64,
    results: Option<SearchOutcome>,
    picked_group: Option<usize>,
    focus: Option<Focus>,
    job: Option<Job>,
    banner: Option<String>,
    preview_tx: Sender<(PathBuf, Preview)>,
    preview_rx: Receiver<(PathBuf, Preview)>,
    loading: HashSet<PathBuf>,
    cache: std::collections::HashMap<PathBuf, Cached>,
    theme: ThemeId,
}

impl DuplicateApp {
    pub fn new(saved_theme: Option<&str>) -> Self {
        let (preview_tx, preview_rx) = mpsc::channel();
        Self {
            root: None,
            min_size: 1,
            by_content: true,
            by_size: true,
            by_name: false,
            by_mtime: false,
            skip_empty: true,
            collapse_hardlinks: true,
            skip_junk: true,
            one_fs: false,
            follow_symlinks: false,
            options_dirty: false,
            filter: String::new(),
            dup_filter: String::new(),
            tab: Tab::Formats,
            inventory: None,
            formats: Vec::new(),
            selected: HashSet::new(),
            stats_dirty: true,
            selected_files: 0,
            selected_bytes: 0,
            results: None,
            picked_group: None,
            focus: None,
            job: None,
            banner: None,
            preview_tx,
            preview_rx,
            loading: HashSet::new(),
            cache: std::collections::HashMap::new(),
            theme: saved_theme
                .and_then(ThemeId::parse)
                .unwrap_or(ThemeId::Lime),
        }
    }

    fn update(&mut self, ctx: &egui::Context) {
        self.apply_look(ctx);
        self.poll(ctx);
        self.dropped_folder(ctx);
        self.recompute_stats();
        self.top_bar(ctx);
        self.preview_panel(ctx);
        self.center(ctx);
        self.status_bar(ctx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let incoming: Vec<_> = std::iter::from_fn(|| self.preview_rx.try_recv().ok()).collect();
        for (path, preview) in incoming {
            self.loading.remove(&path);
            self.store_preview(ctx, path, preview);
        }

        let message = self.job.as_ref().and_then(|job| match job.rx.try_recv() {
            Ok(message) => Some(message),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(JobResult::Inventory(Err(
                "Фоновая задача прервалась.".into(),
            ))),
        });
        if self.job.is_some() && message.is_none() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if let Some(message) = message {
            let kind = self.job.as_ref().map(|job| job.kind);
            self.job = None;
            self.finish(ctx, kind, message);
        }
    }

    fn finish(&mut self, ctx: &egui::Context, kind: Option<JobKind>, message: JobResult) {
        match message {
            JobResult::Inventory(Ok(inventory)) => {
                self.banner = None;
                self.formats = summarize(&inventory.files, self.min_size);
                self.selected = self
                    .formats
                    .iter()
                    .map(|info| info.extension.clone())
                    .collect();
                self.stats_dirty = true;
                self.tab = Tab::Formats;
                let focus = self.formats.first().and_then(|info| {
                    info.samples
                        .first()
                        .map(|sample| focus_from_sample(info, sample))
                });
                self.inventory = Some(inventory);
                if let Some(focus) = focus {
                    self.arm_focus(ctx, focus);
                }
            }
            JobResult::Inventory(Err(err)) => {
                if kind == Some(JobKind::Search) {
                    self.banner = Some(err);
                } else if err == "Остановлено" {
                    self.banner = Some("Просмотр папки остановлен.".into());
                } else {
                    self.banner = Some(err);
                }
            }
            JobResult::Search(Ok(outcome)) => {
                self.banner = None;
                self.picked_group = outcome.groups.first().map(|_| 0);
                if let Some(file) = outcome.groups.first().and_then(|group| group.files.first()) {
                    self.focus_file(ctx, file);
                }
                self.results = Some(outcome);
                self.tab = Tab::Duplicates;
            }
            JobResult::Search(Err(err)) => {
                self.banner = Some(if err == "Остановлено" {
                    "Поиск дублей остановлен.".into()
                } else {
                    err
                });
            }
        }
    }

    fn dropped_folder(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            let folder = if path.is_dir() {
                path
            } else {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            };
            self.start_inventory(ctx, folder);
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("bar")
            .frame(bar_frame(ctx))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let paint = colors(ui.ctx());
                    ui.label(
                        egui::RichText::new("дубли")
                            .size(26.0)
                            .strong()
                            .color(paint.accent),
                    );
                    if ghost_button(ui, "Папка").clicked() {
                        self.pick_folder(ui.ctx());
                    }
                    if let Some(root) = &self.root {
                        ui.label(
                            egui::RichText::new(root.display().to_string()).color(paint.muted),
                        );
                    } else {
                        ui.label(egui::RichText::new("перетащи папку сюда").color(paint.muted));
                    }
                    self.theme_picker(ui);
                });
                ui.horizontal_wrapped(|ui| {
                    let before = self.min_size;
                    egui::ComboBox::from_id_salt("min-size")
                        .selected_text(min_label(self.min_size))
                        .show_ui(ui, |ui| {
                            for &(size, label) in MIN_SIZES {
                                ui.selectable_value(&mut self.min_size, size, label);
                            }
                        });
                    if self.min_size != before {
                        self.rebuild_formats();
                    }
                    if ui
                        .checkbox(&mut self.skip_junk, "без служебных файлов")
                        .changed()
                    {
                        self.options_dirty = true;
                    }
                    if ui
                        .checkbox(&mut self.one_fs, "не уходить на другой диск")
                        .changed()
                    {
                        self.options_dirty = true;
                    }
                    if ui
                        .checkbox(&mut self.follow_symlinks, "переходить по ссылкам")
                        .changed()
                    {
                        self.options_dirty = true;
                    }
                    if self.options_dirty && accent_button(ui, "Обновить список").clicked()
                    {
                        if let Some(root) = self.root.clone() {
                            self.start_inventory(ui.ctx(), root);
                        }
                    }
                    ui.separator();
                    let busy = self.job.is_some();
                    if busy {
                        if stop_button(ui, "Стоп").clicked() {
                            self.cancel_job();
                        }
                    } else {
                        let enabled = self.selected_files > 0 && self.has_rule();
                        let label = format!(
                            "Найти дубли · {} · {}",
                            format_count(self.selected_files),
                            format_bytes(self.selected_bytes)
                        );
                        if ui
                            .add_enabled(enabled, accent_button_widget(label, colors(ui.ctx())))
                            .clicked()
                        {
                            self.start_search(ui.ctx());
                        }
                    }
                });
                self.criteria_row(ui);
                ui.horizontal(|ui| {
                    if pill_tab(ui, self.tab == Tab::Formats, "Форматы").clicked() {
                        self.tab = Tab::Formats;
                    }
                    let dup_label = match &self.results {
                        Some(outcome) => {
                            format!("Дубли {}", format_count(outcome.groups.len() as u64))
                        }
                        None => "Дубли".to_owned(),
                    };
                    if pill_tab(ui, self.tab == Tab::Duplicates, dup_label).clicked() {
                        self.tab = Tab::Duplicates;
                    }
                    ui.label(
                        egui::RichText::new("ничего не удаляем")
                            .size(12.0)
                            .color(colors(ui.ctx()).muted),
                    );
                });
                if let Some(banner) = &self.banner {
                    ui.colored_label(colors(ui.ctx()).stop, banner);
                }
                ui.add_space(2.0);
            });
    }

    fn center(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let inventory_running = self
                .job
                .as_ref()
                .is_some_and(|job| job.kind == JobKind::Inventory);
            if self.inventory.is_none() && inventory_running {
                self.busy(ui);
                return;
            }
            if self.inventory.is_none() {
                self.welcome(ui);
                return;
            }
            match self.tab {
                Tab::Formats => self.formats_tab(ui),
                Tab::Duplicates => self.dups_tab(ui),
            }
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(64.0);
            ui.label(
                egui::RichText::new("дубли")
                    .size(64.0)
                    .strong()
                    .color(colors(ui.ctx()).accent),
            );
            ui.label(
                egui::RichText::new("одинаковые файлы. без удаления.")
                    .size(18.0)
                    .color(colors(ui.ctx()).muted),
            );
            ui.add_space(18.0);
            if accent_button(ui, "Выбрать папку").clicked() {
                self.pick_folder(ui.ctx());
            }
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("или просто перетащи папку в окно")
                    .color(colors(ui.ctx()).muted),
            );
        });
    }

    fn busy(&self, ui: &mut egui::Ui) {
        let Some(job) = &self.job else {
            return;
        };
        ui.vertical_centered(|ui| {
            ui.add_space(56.0);
            ui.spinner();
            match job.kind {
                JobKind::Inventory => {
                    ui.label(
                        egui::RichText::new("смотрю")
                            .size(36.0)
                            .strong()
                            .color(colors(ui.ctx()).accent),
                    );
                    let files = job.progress.files_seen.load(Ordering::Relaxed);
                    let bytes = job.progress.bytes_seen.load(Ordering::Relaxed);
                    ui.label(format!(
                        "{} файлов · {}",
                        format_count(files),
                        format_bytes(bytes)
                    ));
                }
                JobKind::Search => {
                    ui.label(
                        egui::RichText::new("сверяю")
                            .size(36.0)
                            .strong()
                            .color(colors(ui.ctx()).accent),
                    );
                    let done = job.progress.hashed_files.load(Ordering::Relaxed);
                    let total = job.progress.hash_total.load(Ordering::Relaxed);
                    let read = job.progress.bytes_read.load(Ordering::Relaxed);
                    ui.label(format!(
                        "{} / {} файлов · прочитано {}",
                        format_count(done),
                        format_count(total),
                        format_bytes(read)
                    ));
                }
            }
            ui.label(
                egui::RichText::new("окно не зависает. файлы на месте.")
                    .color(colors(ui.ctx()).muted),
            );
        });
    }

    fn formats_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ghost_button(ui, "Все форматы").clicked() {
                self.selected = self
                    .formats
                    .iter()
                    .map(|info| info.extension.clone())
                    .collect();
                self.stats_dirty = true;
            }
            if ghost_button(ui, "Снять выбор").clicked() {
                self.selected.clear();
                self.stats_dirty = true;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Фильтр: jpeg, видео, pdf")
                    .desired_width(240.0),
            );
        });
        let filter = self.filter.to_lowercase();
        let indices: Vec<usize> = self
            .formats
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                filter.is_empty()
                    || info.title.to_lowercase().contains(&filter)
                    || info.extension.to_lowercase().contains(&filter)
                    || info.blurb.to_lowercase().contains(&filter)
            })
            .map(|(idx, _)| idx)
            .collect();
        let mut actions = CardActions::default();
        egui::ScrollArea::vertical()
            .id_salt("formats")
            .show(ui, |ui| {
                if indices.is_empty() {
                    ui.label("В этой папке нет файлов по фильтру.");
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    for idx in indices {
                        ui.scope(|ui| {
                            ui.set_width(236.0);
                            self.draw_card(ui, idx, &mut actions);
                        });
                    }
                });
            });
        self.apply_cards(ui.ctx(), actions);
    }

    fn draw_card(&self, ui: &mut egui::Ui, idx: usize, actions: &mut CardActions) {
        let info = self.formats[idx].clone();
        let checked = self.selected.contains(&info.extension);
        let active = self
            .focus
            .as_ref()
            .is_some_and(|focus| focus.ext == info.extension);
        let paint = colors(ui.ctx());
        let color = kind_color(info.kind, paint.dark);
        let stroke = if active {
            egui::Stroke::new(2.0, paint.accent)
        } else if checked {
            egui::Stroke::new(1.0, color)
        } else {
            egui::Stroke::new(1.0, paint.stroke)
        };
        let fill = if checked { paint.card } else { paint.card_dim };
        egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(18u8)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.set_min_height(220.0);
                ui.horizontal(|ui| {
                    let mut on = checked;
                    if ui.checkbox(&mut on, "").changed() {
                        actions.toggles.push((info.extension.clone(), on));
                    }
                    if ui
                        .selectable_label(
                            active,
                            egui::RichText::new(&info.title)
                                .size(18.0)
                                .strong()
                                .color(paint.text),
                        )
                        .clicked()
                    {
                        if let Some(sample) = info.samples.first() {
                            actions.focus = Some(focus_from_sample(&info, sample));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(&info.extension).small().color(color));
                    });
                });
                ui.label(egui::RichText::new(&info.blurb).small().color(paint.muted));
                ui.label(format!(
                    "{} · {}",
                    files_label(info.files),
                    format_bytes(info.bytes)
                ));
                ui.label(
                    egui::RichText::new(format!(
                        "в поиске: {} · {}",
                        format_count(info.files_in_search),
                        format_bytes(info.bytes_in_search)
                    ))
                    .small(),
                );
                ui.horizontal(|ui| {
                    if info.samples.is_empty() {
                        ui.label("Нет образца");
                        return;
                    }
                    for sample in info.samples.iter().take(3) {
                        self.sample_slot(ui, &info, sample, actions);
                    }
                });
            });
    }

    fn sample_slot(
        &self,
        ui: &mut egui::Ui,
        info: &FormatInfo,
        sample: &Sample,
        actions: &mut CardActions,
    ) {
        let focus_this = Focus {
            path: sample.path.clone(),
            size: sample.size,
            modified: sample.modified,
            ext: info.extension.clone(),
            kind: info.kind,
        };
        let dark = colors(ui.ctx()).dark;
        let response = match self.cache.get(&sample.path) {
            Some(Cached::Image(texture)) => ui.add(
                egui::Image::from_texture((texture.id(), texture.size_vec2()))
                    .max_size(egui::vec2(68.0, 68.0))
                    .maintain_aspect_ratio(true)
                    .sense(egui::Sense::click()),
            ),
            Some(Cached::Text(text)) => {
                let line = text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(18)
                    .collect::<String>();
                ui.add_sized(
                    [68.0, 68.0],
                    egui::Button::new(egui::RichText::new(line).small()),
                )
            }
            Some(Cached::Unavailable(_)) => ui.add_sized(
                [68.0, 68.0],
                egui::Button::new(
                    egui::RichText::new(&info.title)
                        .strong()
                        .color(kind_color(info.kind, dark)),
                ),
            ),
            None => {
                actions.want.push((sample.path.clone(), info.kind));
                ui.add_sized(
                    [68.0, 68.0],
                    egui::Button::new(egui::RichText::new("…").color(kind_color(info.kind, dark))),
                )
            }
        };
        if response.clicked() {
            actions.focus = Some(focus_this);
        }
    }

    fn apply_cards(&mut self, ctx: &egui::Context, actions: CardActions) {
        if !actions.toggles.is_empty() {
            for (ext, on) in actions.toggles {
                if on {
                    self.selected.insert(ext);
                } else {
                    self.selected.remove(&ext);
                }
            }
            self.stats_dirty = true;
        }
        if let Some(focus) = actions.focus {
            self.arm_focus(ctx, focus);
        }
        for (path, kind) in actions.want {
            self.request_preview(ctx, path, kind, false);
        }
    }

    fn dups_tab(&mut self, ui: &mut egui::Ui) {
        let searching = self
            .job
            .as_ref()
            .is_some_and(|job| job.kind == JobKind::Search);
        if searching && self.results.is_none() {
            self.busy(ui);
            return;
        }
        if self.results.is_none() {
            ui.label("Дубли ещё не искали. Отметьте форматы и нажмите «Найти дубли».");
            return;
        }
        let group_count = self
            .results
            .as_ref()
            .map(|outcome| outcome.groups.len())
            .unwrap_or(0);
        let reclaimable: u64 = self
            .results
            .as_ref()
            .map(|outcome| outcome.groups.iter().map(|group| group.reclaimable).sum())
            .unwrap_or(0);
        let hardlinks = self
            .results
            .as_ref()
            .map(|outcome| outcome.hardlinks_collapsed)
            .unwrap_or(0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} групп · можно освободить {}",
                    format_count(group_count as u64),
                    format_bytes(reclaimable)
                ))
                .strong(),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.dup_filter)
                    .hint_text("Фильтр по пути")
                    .desired_width(220.0),
            );
            if ghost_button(ui, "Сохранить отчёт").clicked() {
                self.save_report(ui.ctx());
            }
        });
        if hardlinks > 0 {
            ui.label(format!(
                "Жёсткие ссылки не считаются лишними копиями: {}",
                format_count(hardlinks)
            ));
        }
        let needle = self.dup_filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt("dups").show(ui, |ui| {
            if group_count == 0 {
                ui.label("Одинаковых файлов в выбранных форматах нет.");
                return;
            }
            for idx in 0..group_count {
                let (visible, title) = {
                    let group = &self.results.as_ref().unwrap().groups[idx];
                    let visible = needle.is_empty()
                        || group.files.iter().any(|file| {
                            file.path.to_string_lossy().to_lowercase().contains(&needle)
                        });
                    let title = group_title(group);
                    (visible, title)
                };
                if !visible {
                    continue;
                }
                let selected = self.picked_group == Some(idx);
                if ui
                    .selectable_label(selected, egui::RichText::new(title).strong())
                    .clicked()
                {
                    self.picked_group = Some(idx);
                    let file = self.results.as_ref().unwrap().groups[idx]
                        .files
                        .first()
                        .cloned();
                    if let Some(file) = file {
                        self.focus_file(ui.ctx(), &file);
                    }
                }
                if self.picked_group == Some(idx) {
                    let files = self.results.as_ref().unwrap().groups[idx].files.clone();
                    for file in &files {
                        self.dup_path(ui, file);
                    }
                    ui.add_space(6.0);
                }
            }
        });
    }

    fn dup_path(&mut self, ui: &mut egui::Ui, file: &DupFile) {
        ui.horizontal_wrapped(|ui| {
            let name = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.path.display().to_string());
            if ui.button(name).clicked() {
                self.focus_file(ui.ctx(), file);
            }
            if ui.small_button("в папке").clicked() {
                self.reveal(&file.path);
            }
            if ui.small_button("копировать").clicked() {
                ui.ctx().copy_text(file.path.display().to_string());
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "{} · {}",
                format_mtime(file.modified),
                file.path.display()
            ))
            .small()
            .color(colors(ui.ctx()).muted),
        );
    }

    fn preview_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("preview")
            .resizable(true)
            .default_width(400.0)
            .min_width(280.0)
            .frame(bar_frame(ctx))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("смотри")
                        .size(26.0)
                        .strong()
                        .color(colors(ui.ctx()).accent),
                );
                let Some(focus) = self.focus.clone() else {
                    ui.label("Нажмите формат или файл, чтобы увидеть его здесь.");
                    return;
                };
                self.request_preview(ui.ctx(), focus.path.clone(), focus.kind, true);
                ui.label(egui::RichText::new(focus_title(&focus)).strong());
                ui.label(format!(
                    "{} · {}",
                    format_bytes(focus.size),
                    format_mtime(focus.modified)
                ));
                ui.label(egui::RichText::new(focus.path.display().to_string()).small());
                ui.horizontal(|ui| {
                    if ghost_button(ui, "В папке").clicked() {
                        self.reveal(&focus.path);
                    }
                    if ghost_button(ui, "Копировать путь").clicked() {
                        ui.ctx().copy_text(focus.path.display().to_string());
                    }
                });
                ui.separator();
                match self.cache.get(&focus.path) {
                    Some(Cached::Image(texture)) => {
                        ui.add(
                            egui::Image::from_texture((texture.id(), texture.size_vec2()))
                                .max_size(egui::vec2(ui.available_width(), 420.0))
                                .maintain_aspect_ratio(true),
                        );
                    }
                    Some(Cached::Text(text)) => {
                        egui::ScrollArea::vertical()
                            .id_salt("text-preview")
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(text)
                                        .family(egui::FontFamily::Monospace)
                                        .size(13.0),
                                );
                            });
                    }
                    Some(Cached::Unavailable(message)) => {
                        ui.label(message);
                    }
                    None => {
                        ui.spinner();
                        ui.label("Готовлю предпросмотр…");
                    }
                }
            });
    }

    fn status_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(bar_frame(ctx))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(self.criteria_line())
                            .size(12.0)
                            .color(colors(ui.ctx()).muted),
                    );
                    if let Some(inventory) = &self.inventory {
                        ui.separator();
                        ui.label(format!(
                            "{} файлов · пустых {} · служебных {}",
                            format_count(inventory.files.len() as u64),
                            format_count(inventory.empty_files),
                            format_count(inventory.junk_skipped)
                        ));
                        if inventory.unreadable > 0 {
                            ui.label(format!(
                                "не прочитано: {}",
                                format_count(inventory.unreadable)
                            ));
                        }
                    }
                    if let Some(job) = &self.job {
                        ui.separator();
                        match job.kind {
                            JobKind::Inventory => ui.label("идёт просмотр папки"),
                            JobKind::Search => ui.label("идёт сверка"),
                        };
                    }
                });
            });
    }

    fn criteria_row(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(
            egui::RichText::new("критерии совпадения")
                .strong()
                .color(colors(ui.ctx()).text),
        )
        .id_salt("match-rules")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.by_content, "содержимое")
                    .on_hover_text(
                        "Байты файлов совпадают. Сначала сравнивается размер, затем хеш BLAKE3.",
                    );
                let mut size_flag = self.by_size || self.by_content;
                ui.add_enabled(
                    !self.by_content,
                    egui::Checkbox::new(&mut size_flag, "размер"),
                )
                .on_hover_text(
                    "Без проверки содержимого в одну группу попадают файлы одного размера.",
                );
                if !self.by_content {
                    self.by_size = size_flag;
                }
                ui.checkbox(&mut self.by_name, "имя файла")
                    .on_hover_text("Совпадает имя файла без папки. Регистр учитывается.");
                ui.checkbox(&mut self.by_mtime, "дата изменения")
                    .on_hover_text("Совпадает время изменения с точностью до секунды.");
                if ui
                    .checkbox(&mut self.skip_empty, "пропускать пустые")
                    .on_hover_text("Пустые файлы не считаются дублями.")
                    .changed()
                {
                    self.stats_dirty = true;
                }
                ui.checkbox(&mut self.collapse_hardlinks, "жёсткие ссылки не копии")
                    .on_hover_text("Несколько имён одного файла не занимают место повторно.");
            });
            ui.label(egui::RichText::new(self.criteria_line()).small());
            if !self.by_content {
                ui.label("Содержимое выключено: группа строится только по отмеченным признакам.");
            }
            if !self.has_rule() {
                ui.colored_label(
                    colors(ui.ctx()).stop,
                    "Нужен хотя бы один признак: содержимое, размер, имя или дата.",
                );
            }
        });
    }

    fn has_rule(&self) -> bool {
        self.by_content || self.by_size || self.by_name || self.by_mtime
    }

    fn criteria_line(&self) -> String {
        let mut parts = Vec::new();
        if self.by_content {
            parts.push("содержимое");
        } else if self.by_size {
            parts.push("размер");
        }
        if self.by_name {
            parts.push("имя");
        }
        if self.by_mtime {
            parts.push("дата");
        }
        if self.skip_empty {
            parts.push("без пустых");
        }
        if self.collapse_hardlinks {
            parts.push("жёсткие ссылки не копии");
        }
        if parts.is_empty() {
            "критерии не выбраны".to_owned()
        } else {
            parts.join(" · ")
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Папка для поиска дублей");
        if let Some(root) = &self.root {
            dialog = dialog.set_directory(root);
        }
        if let Some(path) = dialog.pick_folder() {
            self.start_inventory(ctx, path);
        }
    }

    fn start_inventory(&mut self, ctx: &egui::Context, root: PathBuf) {
        self.cancel_job();
        self.root = Some(root.clone());
        self.inventory = None;
        self.formats.clear();
        self.selected.clear();
        self.results = None;
        self.picked_group = None;
        self.focus = None;
        self.cache.clear();
        self.loading.clear();
        self.tab = Tab::Formats;
        self.options_dirty = false;
        self.banner = None;
        self.stats_dirty = true;
        let progress = Progress::new();
        let (tx, rx) = mpsc::channel();
        let opts = self.scan_options();
        let worker_progress = Arc::clone(&progress);
        let worker_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = inventory(&root, &opts, &worker_progress).map_err(|err| err.to_string());
            let _ = tx.send(JobResult::Inventory(result));
            worker_ctx.request_repaint();
        });
        self.job = Some(Job {
            kind: JobKind::Inventory,
            progress,
            rx,
        });
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        let Some(files) = self
            .inventory
            .as_ref()
            .map(|inventory| Arc::clone(&inventory.files))
        else {
            return;
        };
        self.cancel_job();
        self.results = None;
        self.picked_group = None;
        self.tab = Tab::Duplicates;
        self.banner = None;
        let opts = SearchOptions {
            min_size: self.min_size,
            extensions: self.selected.clone(),
            full_hash_below: FULL_HASH_BELOW,
            by_content: self.by_content,
            by_size: self.by_size,
            by_name: self.by_name,
            by_mtime: self.by_mtime,
            skip_empty: self.skip_empty,
            collapse_hardlinks: self.collapse_hardlinks,
        };
        let progress = Progress::new();
        let (tx, rx) = mpsc::channel();
        let worker_progress = Arc::clone(&progress);
        let worker_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = search(&files, &opts, &worker_progress).map_err(|err| err.to_string());
            let _ = tx.send(JobResult::Search(result));
            worker_ctx.request_repaint();
        });
        self.job = Some(Job {
            kind: JobKind::Search,
            progress,
            rx,
        });
    }

    fn cancel_job(&mut self) {
        if let Some(job) = &self.job {
            job.progress.cancel();
        }
    }

    fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            skip_junk: self.skip_junk,
            follow_symlinks: self.follow_symlinks,
            one_file_system: self.one_fs,
        }
    }

    fn rebuild_formats(&mut self) {
        let Some(inventory) = &self.inventory else {
            return;
        };
        self.formats = summarize(&inventory.files, self.min_size);
        let valid: HashSet<_> = self
            .formats
            .iter()
            .map(|info| info.extension.clone())
            .collect();
        self.selected.retain(|ext| valid.contains(ext));
        self.stats_dirty = true;
    }

    fn recompute_stats(&mut self) {
        if !self.stats_dirty {
            return;
        }
        let mut files = 0u64;
        let mut bytes = 0u64;
        if let Some(inventory) = &self.inventory {
            for file in inventory.files.iter() {
                if !self.selected.contains(&file.ext) {
                    continue;
                }
                let take = if file.size == 0 {
                    !self.skip_empty
                } else {
                    file.size >= self.min_size
                };
                if take {
                    files += 1;
                    bytes += file.size;
                }
            }
        }
        self.selected_files = files;
        self.selected_bytes = bytes;
        self.stats_dirty = false;
    }

    fn focus_file(&mut self, ctx: &egui::Context, file: &DupFile) {
        let ext = extension_of(&file.path);
        let (kind, _, _) = classify(&ext);
        self.arm_focus(
            ctx,
            Focus {
                path: file.path.clone(),
                size: file.size,
                modified: file.modified,
                ext,
                kind,
            },
        );
    }

    fn arm_focus(&mut self, ctx: &egui::Context, focus: Focus) {
        self.request_preview(ctx, focus.path.clone(), focus.kind, true);
        self.focus = Some(focus);
    }

    fn request_preview(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        kind: FormatKind,
        force: bool,
    ) {
        if self.cache.contains_key(&path) || self.loading.contains(&path) {
            return;
        }
        if !force && self.loading.len() >= 3 {
            return;
        }
        self.loading.insert(path.clone());
        let tx = self.preview_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let preview = load_preview(&path, kind, 480);
            let _ = tx.send((path, preview));
            ctx.request_repaint();
        });
    }

    fn store_preview(&mut self, ctx: &egui::Context, path: PathBuf, preview: Preview) {
        let cached = match preview {
            Preview::Image {
                width,
                height,
                rgba,
            } => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &rgba,
                );
                let texture = ctx.load_texture(
                    path.display().to_string(),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                Cached::Image(texture)
            }
            Preview::Text(text) => Cached::Text(text),
            Preview::Unavailable(text) => Cached::Unavailable(text),
        };
        self.cache.insert(path.clone(), cached);
        if self.cache.len() > 80 {
            let stale = self.cache.keys().find(|key| *key != &path).cloned();
            if let Some(stale) = stale {
                self.cache.remove(&stale);
            }
        }
    }

    fn save_report(&mut self, ctx: &egui::Context) {
        let Some(outcome) = self.results.as_ref() else {
            return;
        };
        let reclaimable = outcome
            .groups
            .iter()
            .map(|group| group.reclaimable)
            .sum::<u64>();
        let report = serde_json::json!({
            "root": self.root,
            "min_size": self.min_size,
            "criteria": {
                "content": self.by_content,
                "size": self.by_size || self.by_content,
                "name": self.by_name,
                "modified": self.by_mtime,
                "skip_empty": self.skip_empty,
                "hardlinks_are_not_copies": self.collapse_hardlinks,
            },
            "files_considered": outcome.files_considered,
            "hardlinks_collapsed": outcome.hardlinks_collapsed,
            "reclaimable_bytes": reclaimable,
            "groups": outcome.groups,
        });
        let Some(path) = rfd::FileDialog::new()
            .set_title("Сохранить отчёт")
            .set_file_name("duplicates.json")
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        match serde_json::to_string_pretty(&report) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&path, text) {
                    self.banner = Some(format!("Не удалось записать отчёт: {err}"));
                } else {
                    self.banner = Some(format!("Отчёт сохранён: {}", path.display()));
                }
            }
            Err(err) => self.banner = Some(err.to_string()),
        }
        ctx.request_repaint();
    }

    fn theme_picker(&mut self, ui: &mut egui::Ui) {
        let before = self.theme;
        let paint = colors(ui.ctx());
        let response = egui::ComboBox::from_id_salt("theme")
            .selected_text(
                egui::RichText::new(self.theme.label())
                    .strong()
                    .color(paint.accent),
            )
            .show_ui(ui, |ui| {
                for theme in ThemeId::ALL {
                    ui.selectable_value(&mut self.theme, theme, theme.label());
                }
            });
        response.response.on_hover_text("цветовая тема");
        if self.theme != before {
            self.apply_look(ui.ctx());
        }
    }

    pub fn apply_look(&self, ctx: &egui::Context) {
        install_look(ctx, self.theme);
    }

    fn reveal(&mut self, path: &Path) {
        if let Err(err) = reveal_in_file_manager(path) {
            self.banner = Some(err);
        }
    }
}

impl Drop for DuplicateApp {
    fn drop(&mut self) {
        self.cancel_job();
    }
}

impl eframe::App for DuplicateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.key().to_owned());
    }
}

fn focus_from_sample(info: &FormatInfo, sample: &Sample) -> Focus {
    Focus {
        path: sample.path.clone(),
        size: sample.size,
        modified: sample.modified,
        ext: info.extension.clone(),
        kind: info.kind,
    }
}

fn focus_title(focus: &Focus) -> String {
    let (_, title, _) = classify(&focus.ext);
    let name = focus
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| focus.path.display().to_string());
    format!("{title} · {name}")
}

fn group_title(group: &DupGroup) -> String {
    let same_size = group
        .files
        .first()
        .is_some_and(|file| group.files.iter().all(|other| other.size == file.size));
    if same_size {
        format!(
            "{} × {} · лишние {}",
            format_bytes(group.size),
            group.copies,
            format_bytes(group.reclaimable)
        )
    } else {
        format!(
            "{}, размеры разные · лишние {}",
            files_label(group.copies as u64),
            format_bytes(group.reclaimable)
        )
    }
}

fn files_label(count: u64) -> String {
    let word = match count % 100 {
        11..=14 => "файлов",
        _ => match count % 10 {
            1 => "файл",
            2..=4 => "файла",
            _ => "файлов",
        },
    };
    format!("{} {word}", format_count(count))
}

fn min_label(size: u64) -> &'static str {
    MIN_SIZES
        .iter()
        .find(|(value, _)| *value == size)
        .map(|(_, label)| *label)
        .unwrap_or("размер")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeId {
    Lime,
    Berry,
    Sea,
    Sunset,
    Day,
}

impl ThemeId {
    const ALL: [ThemeId; 5] = [
        ThemeId::Lime,
        ThemeId::Berry,
        ThemeId::Sea,
        ThemeId::Sunset,
        ThemeId::Day,
    ];

    fn label(self) -> &'static str {
        match self {
            ThemeId::Lime => "лайм",
            ThemeId::Berry => "ягода",
            ThemeId::Sea => "море",
            ThemeId::Sunset => "закат",
            ThemeId::Day => "день",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ThemeId::Lime => "lime",
            ThemeId::Berry => "berry",
            ThemeId::Sea => "sea",
            ThemeId::Sunset => "sunset",
            ThemeId::Day => "day",
        }
    }

    fn parse(value: &str) -> Option<ThemeId> {
        Self::ALL.into_iter().find(|theme| theme.key() == value)
    }

    fn palette(self) -> Palette {
        let rgb = egui::Color32::from_rgb;
        match self {
            ThemeId::Lime => Palette {
                bg: rgb(12, 12, 16),
                card: rgb(26, 26, 34),
                card_dim: rgb(18, 18, 24),
                text: rgb(244, 244, 246),
                muted: rgb(154, 154, 168),
                accent: rgb(214, 255, 74),
                ink: rgb(16, 16, 16),
                stroke: rgb(48, 48, 62),
                stop: rgb(255, 107, 129),
                extreme: rgb(8, 8, 12),
                selection: rgb(62, 74, 24),
                hover: rgb(36, 36, 48),
                active: rgb(54, 64, 22),
                dark: true,
            },
            ThemeId::Berry => Palette {
                bg: rgb(18, 10, 16),
                card: rgb(36, 20, 32),
                card_dim: rgb(24, 14, 22),
                text: rgb(255, 236, 244),
                muted: rgb(196, 150, 170),
                accent: rgb(255, 92, 168),
                ink: rgb(32, 8, 18),
                stroke: rgb(72, 40, 58),
                stop: rgb(255, 140, 150),
                extreme: rgb(10, 6, 10),
                selection: rgb(92, 28, 58),
                hover: rgb(52, 28, 44),
                active: rgb(84, 24, 52),
                dark: true,
            },
            ThemeId::Sea => Palette {
                bg: rgb(8, 16, 24),
                card: rgb(16, 32, 44),
                card_dim: rgb(10, 22, 32),
                text: rgb(232, 248, 248),
                muted: rgb(140, 176, 184),
                accent: rgb(94, 234, 212),
                ink: rgb(6, 28, 28),
                stroke: rgb(36, 64, 76),
                stop: rgb(255, 107, 129),
                extreme: rgb(4, 10, 16),
                selection: rgb(20, 72, 68),
                hover: rgb(22, 48, 60),
                active: rgb(18, 72, 64),
                dark: true,
            },
            ThemeId::Sunset => Palette {
                bg: rgb(24, 14, 10),
                card: rgb(42, 26, 18),
                card_dim: rgb(30, 18, 12),
                text: rgb(255, 236, 220),
                muted: rgb(196, 160, 136),
                accent: rgb(255, 150, 64),
                ink: rgb(36, 16, 4),
                stroke: rgb(78, 48, 32),
                stop: rgb(255, 107, 129),
                extreme: rgb(14, 8, 6),
                selection: rgb(92, 48, 16),
                hover: rgb(58, 34, 22),
                active: rgb(92, 48, 18),
                dark: true,
            },
            ThemeId::Day => Palette {
                bg: rgb(246, 244, 239),
                card: rgb(255, 255, 255),
                card_dim: rgb(236, 233, 226),
                text: rgb(28, 27, 31),
                muted: rgb(110, 106, 116),
                accent: rgb(109, 76, 255),
                ink: rgb(255, 255, 255),
                stroke: rgb(214, 210, 200),
                stop: rgb(190, 36, 68),
                extreme: rgb(228, 224, 216),
                selection: rgb(226, 220, 255),
                hover: rgb(236, 232, 250),
                active: rgb(214, 206, 255),
                dark: false,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct Palette {
    bg: egui::Color32,
    card: egui::Color32,
    card_dim: egui::Color32,
    text: egui::Color32,
    muted: egui::Color32,
    accent: egui::Color32,
    ink: egui::Color32,
    stroke: egui::Color32,
    stop: egui::Color32,
    extreme: egui::Color32,
    selection: egui::Color32,
    hover: egui::Color32,
    active: egui::Color32,
    dark: bool,
}

fn colors(ctx: &egui::Context) -> Palette {
    ctx.data(|data| data.get_temp(egui::Id::new("dup-theme")))
        .unwrap_or_else(|| ThemeId::Lime.palette())
}

fn install_look(ctx: &egui::Context, theme: ThemeId) {
    let paint = theme.palette();
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("dup-theme"), paint));
    let mut visuals = if paint.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.window_fill = paint.bg;
    visuals.panel_fill = paint.bg;
    visuals.faint_bg_color = paint.card;
    visuals.extreme_bg_color = paint.extreme;
    visuals.code_bg_color = paint.card;
    visuals.hyperlink_color = paint.accent;
    visuals.selection.bg_fill = paint.selection;
    visuals.selection.stroke = egui::Stroke::new(1.0, paint.accent);
    visuals.warn_fg_color = paint.stop;
    visuals.error_fg_color = paint.stop;
    visuals.window_corner_radius = egui::CornerRadius::same(16);
    visuals.menu_corner_radius = egui::CornerRadius::same(12);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(12);
    }
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, paint.text);
    visuals.widgets.inactive.bg_fill = paint.card;
    visuals.widgets.inactive.weak_bg_fill = paint.card;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, paint.text);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, paint.stroke);
    visuals.widgets.hovered.bg_fill = paint.hover;
    visuals.widgets.hovered.weak_bg_fill = paint.hover;
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, paint.text);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, paint.accent);
    visuals.widgets.active.bg_fill = paint.active;
    visuals.widgets.active.weak_bg_fill = paint.active;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, paint.accent);
    if !paint.dark {
        visuals.widgets.noninteractive.bg_fill = paint.bg;
    }
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    ctx.set_style(style);
}

fn bar_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::new()
        .fill(colors(ctx).bg)
        .inner_margin(egui::Margin::symmetric(14, 10))
}

fn accent_button_widget(text: impl Into<String>, paint: Palette) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(paint.ink).strong())
        .fill(paint.accent)
        .corner_radius(16u8)
}

fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(accent_button_widget(text, colors(ui.ctx())))
}

fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let paint = colors(ui.ctx());
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(paint.text))
            .fill(paint.card)
            .stroke(egui::Stroke::new(1.0, paint.stroke))
            .corner_radius(16u8),
    )
}

fn stop_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .color(egui::Color32::WHITE)
                .strong(),
        )
        .fill(colors(ui.ctx()).stop)
        .corner_radius(16u8),
    )
}

fn pill_tab(ui: &mut egui::Ui, selected: bool, text: impl Into<String>) -> egui::Response {
    let text = text.into();
    if selected {
        accent_button(ui, &text)
    } else {
        ghost_button(ui, &text)
    }
}

fn kind_color(kind: FormatKind, dark: bool) -> egui::Color32 {
    let rgb = egui::Color32::from_rgb;
    if dark {
        match kind {
            FormatKind::Image => rgb(125, 211, 252),
            FormatKind::Video => rgb(196, 181, 253),
            FormatKind::Audio => rgb(253, 186, 116),
            FormatKind::Document => rgb(134, 239, 172),
            FormatKind::Archive => rgb(252, 211, 77),
            FormatKind::Text => rgb(103, 232, 249),
            FormatKind::Other => rgb(168, 168, 184),
        }
    } else {
        match kind {
            FormatKind::Image => rgb(2, 132, 199),
            FormatKind::Video => rgb(124, 58, 237),
            FormatKind::Audio => rgb(234, 88, 12),
            FormatKind::Document => rgb(22, 163, 74),
            FormatKind::Archive => rgb(180, 120, 4),
            FormatKind::Text => rgb(8, 145, 178),
            FormatKind::Other => rgb(82, 82, 91),
        }
    }
}

fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    let result = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg("-R")
                .arg(path)
                .spawn()
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("explorer")
                .arg(format!("/select,{}", path.display()))
                .spawn()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let folder = path.parent().unwrap_or(path);
            std::process::Command::new("xdg-open").arg(folder).spawn()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "нет команды, чтобы открыть папку",
            ))
        }
    };
    result.map(|_| ()).map_err(|err| err.to_string())
}
