use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{atomic::AtomicUsize, Arc, Mutex},
    thread,
};

#[derive(Debug)]
pub struct Metric {
    number: AtomicUsize,
    logs: Mutex<Vec<String>>,
    print_file: Mutex<Option<BufWriter<File>>>,
    print_file_path: Option<PathBuf>,
    ordering: std::sync::atomic::Ordering,
}

impl Default for Metric {
    fn default() -> Self {
        Self::new(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Metric {
    pub fn new(ordering: std::sync::atomic::Ordering) -> Self {
        Self {
            number: AtomicUsize::new(0),
            logs: Mutex::new(Vec::new()),
            ordering,
            print_file: Mutex::new(None),
            print_file_path: None,
        }
    }

    pub fn set_print_file(mut self, print_file: PathBuf) -> Self {
        self.print_file_path = Some(print_file);
        self
    }

    pub fn increment(&self) {
        self.number.fetch_add(1, self.ordering);
    }

    pub fn decrement(&self) {
        self.number.fetch_sub(1, self.ordering);
    }

    pub fn set(&self, value: usize) {
        self.number.store(value, self.ordering);
    }

    pub fn get(&self) -> usize {
        self.number.load(self.ordering)
    }

    pub fn add(&self, value: usize) {
        self.number.fetch_add(value, self.ordering);
    }

    pub fn sub(&self, value: usize) {
        self.number.fetch_sub(value, self.ordering);
    }

    pub fn push(&self, log: String) {
        if let Some(print_file_path) = &self.print_file_path {
            let mut print_file = self.print_file.lock().unwrap();
            if print_file.is_none() {
                let print_file_buf = File::create(print_file_path).unwrap();
                *print_file = Some(BufWriter::new(print_file_buf));
            }
            let print_file_buf = print_file.as_mut().unwrap();
            print_file_buf
                .write_all(format!("{}\n", log).as_bytes())
                .unwrap();
        }
        self.logs.lock().unwrap().push(log);
    }

    pub fn update(&self, index: usize, new_log: String) {
        let mut logs = self.logs.lock().unwrap();
        if let Some(log) = logs.get_mut(index) {
            *log = new_log;
        } else {
            logs.push(new_log);
        }
    }

    pub fn consumers(&self) -> Vec<String> {
        // 取出所有的日志，并且清空
        self.logs.lock().unwrap().drain(..).collect()
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Indicator {
    pub metric: HashMap<String, Arc<Metric>>,
}

impl Indicator {
    pub fn new() -> Self {
        Self {
            metric: HashMap::new(),
        }
    }

    pub fn init(mut self, metrics: Vec<String>, print_file: String) -> Self {
        let mut metric = HashMap::new();
        for m in metrics {
            // if m.contains("progress") {
            //     metric.insert(
            //         m.clone(),
            //         Arc::new(Metric::new(std::sync::atomic::Ordering::SeqCst)),
            //     );
            // } else {
            metric.insert(
                m.clone(),
                Arc::new(Metric::default().set_print_file(PathBuf::from(format!(
                    "./.mongobar/{}/{}.log",
                    print_file, m
                )))),
            );
            // }
        }
        self.metric = metric;
        self
    }

    pub fn take(&self, name: &str) -> Option<Arc<Metric>> {
        if let Some(v) = self.metric.get(name).map(|m| Arc::clone(m)) {
            Some(v)
        } else {
            Some(Arc::new(Metric::default()))
        }
    }

    pub fn reset(&self) {
        for (_, v) in self.metric.iter() {
            v.set(0);
            v.consumers();
        }
    }
}

pub fn print_indicator(indicator: &Indicator) {
    let boot_worker = indicator.take("boot_worker").unwrap();
    let query_count = indicator.take("query_count").unwrap();
    // let in_size = Arc::new(AtomicUsize::new(0));
    // let out_size = Arc::new(AtomicUsize::new(0));
    let cost_ms = indicator.take("cost_ms").unwrap();
    let progress = indicator.take("progress").unwrap();
    let logs = indicator.take("logs").unwrap();
    let progress_total = indicator.take("progress_total").unwrap();
    let thread_count = indicator.take("thread_count").unwrap();

    thread::spawn({
        let query_count = query_count.clone();
        // let in_size = in_size.clone();
        // let out_size = out_size.clone();
        let progress = progress.clone();
        let cost_ms = cost_ms.clone();
        let boot_worker = boot_worker.clone();
        let logs = logs.clone();
        let progress_total = progress_total.clone();
        let thread_count = thread_count.clone();
        move || {
            let mut last_query_count = 0;
            // let mut last_in_size = 0;
            // let mut last_out_size = 0;

            loop {
                let progress_total = progress_total.get();
                let thread_count = thread_count.get();
                thread::sleep(tokio::time::Duration::from_secs(1));
                let query_count = query_count.get();
                // let in_size = in_size.load(Ordering::Relaxed);
                // let out_size = out_size.load(Ordering::Relaxed);
                let progress = progress.get();
                let current_progress = (progress as f64 / progress_total as f64) * 100.0;
                let cost_ms = cost_ms.get();
                let boot_worker = boot_worker.get();
                if boot_worker < thread_count as usize {
                    println!(
                        "IND [{}] wait for boot {}/{}.",
                        chrono::Local::now().timestamp(),
                        boot_worker,
                        thread_count
                    );
                    continue;
                }
                logs.consumers().iter().for_each(|v| {
                    println!("{}", v);
                });

                // println!(
                //     "IND [{}] query_count: {} in_size: {} out_size: {}",
                //     chrono::Local::now().timestamp(),
                //     query_count,
                //     in_size,
                //     out_size
                // );
                // println!(
                //     "IND [{}] count: {}/s io: ({:.2},{:.2})MB/s cost: {:.2}/ms progress: {:.2}%",
                //     chrono::Local::now().timestamp(),
                //     query_count - last_query_count,
                //     bytes_to_mb(in_size - last_in_size),
                //     bytes_to_mb(out_size - last_out_size),
                //     (cost_ms as f64 / query_count as f64),
                //     current_progress
                // );
                println!(
                    "IND [{}] count: {}/s cost: {:.2}ms progress: {:.2}% {}/{}",
                    chrono::Local::now().timestamp(),
                    query_count - last_query_count,
                    (cost_ms as f64 / query_count as f64),
                    current_progress,
                    progress,
                    progress_total
                );
                last_query_count = query_count;
                // last_in_size = in_size;
                // last_out_size = out_size;
            }
        }
    });
}
