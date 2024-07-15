use std::{
    fs::{self},
    path::PathBuf,
    sync::Arc,
};

use bson::{doc, DateTime};

use mongodb::{bson::Document, options::ClientOptions, Client, Collection, Cursor};

use regex::Regex;
use tokio::time::Instant;

use crate::indicator::Indicator;

mod mongobar_config;

mod op_state;

pub mod op_logs;
pub mod op_row;

#[derive(Clone, Debug, Default)]
pub(crate) struct Mongobar {
    pub(crate) dir: PathBuf,
    pub(crate) name: String,

    pub(crate) op_workdir: PathBuf,
    pub(crate) op_logs: Arc<op_logs::OpLogs>,
    pub(crate) op_file_padding: PathBuf,
    pub(crate) op_file_done: PathBuf,
    pub(crate) op_file_resume: PathBuf,

    pub(crate) op_state_file: PathBuf,
    pub(crate) op_state: op_state::OpState,

    pub(crate) config_file: PathBuf,
    pub(crate) config: mongobar_config::MongobarConfig,

    pub(crate) indicator: Indicator,
    pub(crate) signal: Arc<crate::signal::Signal>,

    pub(crate) op_filter: Option<Regex>,
}

impl Mongobar {
    pub fn new(name: &str) -> Self {
        let cur_cwd: PathBuf = std::env::current_dir().unwrap();
        let dir: PathBuf = cur_cwd.join("runtime");
        let workdir: PathBuf = dir.join(name);
        let op_file_padding = workdir.join(PathBuf::from("padding.op"));
        Self {
            name: name.to_string(),
            op_workdir: workdir.clone(),
            op_logs: Arc::new(op_logs::OpLogs::new(op_file_padding.clone())),
            op_file_padding,
            op_file_done: workdir.join(PathBuf::from("done.op")),
            op_file_resume: workdir.join(PathBuf::from("resume.op")),
            config_file: cur_cwd.join(PathBuf::from("mongobar.json")),
            config: mongobar_config::MongobarConfig::default(),
            dir,

            op_state_file: workdir.join(PathBuf::from("state.json")),
            op_state: op_state::OpState::default(),
            indicator: Indicator::new(),

            signal: Arc::new(crate::signal::Signal::new()),

            op_filter: None,
        }
    }

    pub fn cwd(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    pub fn init(mut self) -> Self {
        let cwd = self.cwd();

        if !cwd.exists() {
            fs::create_dir_all(&cwd).unwrap();
            fs::write(cwd.clone().join(&self.op_file_padding), "").unwrap();
            fs::write(cwd.clone().join(&self.op_file_done), "").unwrap();
        }

        self.load_config();
        self.load_state();
        // self.load_op_rows();

        return self;
    }

    pub fn set_filter(mut self, filter: Option<String>) -> Self {
        if let Some(filter) = filter {
            self.op_filter = Some(Regex::new(&filter).unwrap());
        }
        self
    }

    pub fn set_indicator(mut self, indicator: Indicator) -> Self {
        self.indicator = indicator;
        self
    }

    pub fn set_signal(mut self, signal: Arc<crate::signal::Signal>) -> Self {
        self.signal = signal;
        self
    }

    pub fn clean(self) -> Self {
        let _ = fs::remove_dir_all(&self.cwd());
        Self::new(&self.name).init()
    }

    pub fn load_config(&mut self) {
        if !self.config_file.exists() {
            self.save_config();
        }
        let content: String = fs::read_to_string(&self.config_file).unwrap();
        self.config = serde_json::from_str(&content).unwrap();
    }

    pub fn save_config(&self) {
        let content = serde_json::to_string(&self.config).unwrap();
        fs::write(&self.config_file, content).unwrap();
    }

    pub fn load_state(&mut self) {
        if !self.op_state_file.exists() {
            self.save_state();
        }
        let content = fs::read_to_string(&self.op_state_file).unwrap();
        self.op_state = serde_json::from_str(&content).unwrap();
    }

    pub fn save_state(&self) {
        let content: String = serde_json::to_string(&self.op_state).unwrap();
        fs::write(&self.op_state_file, content).unwrap();
    }

    pub fn add_row_by_profile(&mut self, doc: &Document) {
        let ns = doc.get_str("ns").unwrap().to_string();
        if ns.contains("system.profile") {
            return;
        }
        // let doc_as_json = serde_json::to_string(&doc).unwrap();
        // println!("{}", doc_as_json);
        let mut row = op_row::OpRow::default();
        let op = doc.get_str("op").unwrap();
        let command = doc.get_document("command").unwrap();
        match op {
            "query" => {
                if let Err(_) = doc.get_str("queryHash") {
                    return;
                }
                row.id = doc.get_str("queryHash").unwrap().to_string();
                row.ns = ns;
                row.ts = doc.get_datetime("ts").unwrap().timestamp_millis() as i64;
                row.op = op_row::Op::Query;
                row.db = command.get_str("$db").unwrap().to_string();
                row.coll = command.get_str("find").unwrap().to_string();
                let mut new_cmd = command.clone();
                new_cmd.remove("lsid");
                new_cmd.remove("$clusterTime");
                new_cmd.remove("$db");
                row.cmd = new_cmd
            }
            _ => {}
        }
        // println!("{:?}", row);
        self.op_logs.push(row);
    }

    // pub fn load_op_rows(&mut self) {
    //     let content = fs::read_to_string(&self.op_file_padding).unwrap();
    //     let rows: Vec<op_row::OpRow> = content
    //         .split("\n")
    //         .filter(|v| !v.is_empty())
    //         .filter(|v| {
    //             if let Some(filter) = &self.op_filter {
    //                 return filter.is_match(v);
    //             }
    //             return true;
    //         })
    //         .map(|v| serde_json::from_str(v).unwrap())
    //         .collect();

    //     self.op_logs = rows;
    // }
    /// 录制逻辑：
    /// 1. 【程序】标记开始时间 毫秒
    /// 2. 【人工】操作具体业务
    /// 3. 【程序】标记结束时间 毫秒
    /// 4. 【程序】读取 oplog.rs 中的数据，找到对应的操作
    /// 5. 【程序】读取 db.system.profile 中的数据，找到对应的操作
    /// 6. 【程序】处理两个数据，并且按时间排序，最终生成可以执行的逻辑，生成文件
    pub async fn op_record(
        &mut self,
        time_range: (DateTime, DateTime),
    ) -> Result<(), anyhow::Error> {
        if self.op_state.record_end_ts > 0 {
            panic!("[OPRecord] 已经录制过了，不能重复录制，请先调用 clean 清理数据");
        }

        let start_time = time_range.0;
        let end_time = time_range.1;
        let client = Client::with_uri_str(&self.config.uri).await?;

        let db = client.database(&self.config.db);

        let c: Collection<Document> = db.collection("system.profile");

        let ns_ne = self.config.db.clone() + ".system.profile";

        let query = doc! {
           "op": "query",
           "ns": { "$ne": ns_ne },
           "ts": { "$gte": start_time, "$lt": end_time }
        };
        // let doc_as_json = serde_json::to_string(&query)?;
        // println!("{}", doc_as_json);
        let mut cursor: Cursor<Document> = c.find(query).await?;

        while cursor.advance().await? {
            let v = cursor.deserialize_current().unwrap();
            self.add_row_by_profile(&v);
            // let doc_as_json = serde_json::to_string(&v)?;
            // println!("{}", doc_as_json);
        }

        self.op_state.record_start_ts = start_time.timestamp_millis() as i64;
        self.op_state.record_end_ts = end_time.timestamp_millis() as i64;
        self.save_state();

        Ok(())
    }

    // 执行录制好的压测文件：
    // 1. 【程序】读取文件
    // 2. 【程序】创建 1000 个线程，并预分配好每个线程的操作
    // 3. 【程序】标记开始时间 毫秒
    // 4. 【程序】放开所有线程
    // 5. 【程序】等待所有线程结束
    // 6. 【程序】标记结束时间 毫秒
    // 7. 【程序】计算分析
    pub async fn op_stress(&self) -> Result<(), anyhow::Error> {
        // let record_start_time = DateTime::from_millis(self.op_state.record_start_ts);
        // let record_end_time = DateTime::from_millis(self.op_state.record_end_ts);

        let loop_count = self.config.loop_count;
        let thread_count = self.config.thread_count;

        let mongo_uri = self.config.uri.clone();
        {
            let options = ClientOptions::parse(&mongo_uri).await.unwrap();
            let client = Client::with_options(options).unwrap();
            let db = client.database(&self.config.db);

            let cur_profile = db.run_command(doc! {  "profile": -1 }).await?;

            if let Ok(was) = cur_profile.get_i32("was") {
                if was != 0 {
                    db.run_command(doc! { "profile": 0 }).await?;
                }
            }
            client.shutdown().await;
        }

        // println!(
        //     "OPStress [{}] loop_count: {} thread_count: {}",
        //     chrono::Local::now().timestamp(),
        //     loop_count,
        //     thread_count
        // );

        let gate = Arc::new(tokio::sync::Barrier::new(thread_count as usize));
        let mut handles = vec![];

        let dyn_threads = self.indicator.take("dyn_threads").unwrap();
        let dyn_cc_limit = self.indicator.take("dyn_cc_limit").unwrap();

        let boot_worker = self.indicator.take("boot_worker").unwrap();
        let done_worker = self.indicator.take("done_worker").unwrap();
        let query_count = self.indicator.take("query_count").unwrap();
        let query_qps = self.indicator.take("query_qps").unwrap();
        let querying = self.indicator.take("querying").unwrap();
        // let in_size = Arc::new(AtomicUsize::new(0));
        // let out_size = Arc::new(AtomicUsize::new(0));
        let cost_ms = self.indicator.take("cost_ms").unwrap();
        let progress = self.indicator.take("progress").unwrap();
        let logs = self.indicator.take("logs").unwrap();
        let signal = Arc::clone(&self.signal);

        self.indicator
            .take("thread_count")
            .unwrap()
            .set(thread_count as usize);

        // thread::spawn({
        //     let signal = Arc::clone(&signal);
        //     let query_count = query_count.clone();
        //     let query_qps = query_qps.clone();
        //     move || {
        //         let mut last_query_count = 0;
        //         loop {
        //             std::thread::sleep(std::time::Duration::from_secs(1));
        //             let cur_query_count = query_count.get();
        //             let qps = cur_query_count - last_query_count;
        //             query_qps.set(qps);
        //             last_query_count = query_count.get();
        //             if signal.get() != 0 {
        //                 break;
        //             }
        //         }
        //     }
        // });

        let mut client_pool = ClientPool::new(&self.config.uri, thread_count * 100);

        let mut created_thread_count = 0;
        loop {
            let dyn_threads_num = dyn_threads.get();
            let thread_count_total = thread_count as i32 + dyn_threads_num as i32;
            let done_worker_num = done_worker.get();
            if done_worker_num >= thread_count_total as usize {
                break;
            }
            if signal.get() != 0 {
                break;
            }
            if created_thread_count >= thread_count_total {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }
            // -------------------------------------
            let thread_index = created_thread_count as usize;
            let gate = gate.clone();
            let op_rows = self.op_logs.clone();

            // let out_size = out_size.clone();
            // let in_size = in_size.clone();
            let query_count = query_count.clone();
            let progress = progress.clone();
            let cost_ms = cost_ms.clone();
            let boot_worker = boot_worker.clone();
            let logs = logs.clone();
            let client = client_pool.get().await?;
            let signal = Arc::clone(&signal);
            let done_worker = done_worker.clone();
            let dyn_cc_limit = dyn_cc_limit.clone();
            let query_qps = query_qps.clone();
            let querying = querying.clone();
            let thread_count_num = thread_count;

            handles.push(tokio::spawn(async move {
                // println!("Thread[{}] [{}]\twait", i, chrono::Local::now().timestamp());
                boot_worker.increment();
                if thread_index < thread_count_num as usize {
                    gate.wait().await;
                }
                // println!(
                //     "Thread[{}] [{}]\tstart",
                //     i,
                //     chrono::Local::now().timestamp()
                // );

                // let client = Client::with_uri_str(mongo_uri).await.unwrap();
                let mut index = 0 as usize;

                loop {
                    if loop_count != -1 {
                        index += 1;
                        if index > loop_count as usize {
                            break;
                        }
                    }
                    if signal.get() != 0 {
                        break;
                    }
                    let dyn_cc_limit_n = dyn_cc_limit.get();
                    if dyn_cc_limit_n > 0 && querying.get() >= dyn_cc_limit_n {
                        let rand = rand::random::<u64>() % 100;
                        tokio::time::sleep(tokio::time::Duration::from_millis(rand)).await;
                        continue;
                    }
                    let mut row_index = 0;
                    while let Some(row) = op_rows.iter(thread_index, row_index) {
                        if signal.get() != 0 {
                            break;
                        }
                        querying.increment();
                        progress.increment();
                        match &row.op {
                            op_row::Op::Query => {
                                let db = client.database(&row.db);
                                // out_size.fetch_add(row.cmd.len(), Ordering::Relaxed);
                                let start = Instant::now();
                                let res = db.run_cursor_command(row.cmd.clone()).await;
                                let end = start.elapsed();
                                cost_ms.add(end.as_millis() as usize);
                                query_count.increment();
                                if let Err(e) = &res {
                                    logs.push(format!(
                                        "OPStress [{}] [{}]\t err {}",
                                        chrono::Local::now().timestamp(),
                                        thread_index,
                                        e
                                    ));
                                }
                                // if let Ok(mut cursor) = res {
                                //     let mut sum = 0;
                                //     while cursor.advance().await.unwrap() {
                                //         sum += cursor.current().as_bytes().len();
                                //     }
                                //     in_size.fetch_add(sum, Ordering::Relaxed);
                                // }
                            }
                            _ => {}
                        }

                        querying.decrement();
                        row_index += 1;
                    }
                }

                // println!("Thread[{}] [{}]\tend", i, chrono::Local::now().timestamp());

                done_worker.increment();
            }));
            created_thread_count += 1;
            if loop_count == -1 {
                self.indicator.take("progress_total").unwrap().set(0);
            } else {
                self.indicator.take("progress_total").unwrap().set(
                    self.op_logs.len()
                        * loop_count as usize
                        * (thread_count as usize + dyn_threads_num),
                );
            }
        }

        // let stress_start_time: i64 = chrono::Local::now().timestamp();
        // self.op_state.stress_start_ts = stress_start_time;
        // self.save_state();

        for handle in handles {
            handle.await?;
        }

        // let stress_end_time = chrono::Local::now().timestamp();
        // self.op_state.stress_end_ts = stress_end_time;
        // self.save_state();

        // if let Ok(was) = cur_profile.get_i32("was") {
        //     if was != 0 {
        //         db.run_command(doc! { "profile": was }).await?;
        //     }
        // }

        client_pool.shutdown().await;

        Ok(())
    }

    // 恢复压测前状态
    // 1. 【程序】读取上面标记的时间
    // 2. 【程序】通过时间拉取所有的 oplog.rs
    // 3. 【程序】反向执行所有的操作
    // pub async fn op_resume(&self) -> Result<(), anyhow::Error> {
    //     Ok(())
    // }
}

// fn bytes_to_mb(bytes: usize) -> f64 {
//     bytes as f64 / 1024.0 / 1024.0
// }

struct ClientPool {
    uri: String,
    clients: Vec<Arc<Client>>,
    every_size: u32,
    get_index: usize,
}

impl ClientPool {
    fn new(uri: &str, every_size: u32) -> Self {
        let clients = vec![];

        Self {
            clients,
            every_size,
            uri: uri.to_string(),
            get_index: 0,
        }
    }

    async fn get(&mut self) -> Result<Arc<Client>, anyhow::Error> {
        let len = self.clients.len();
        let total = len * self.every_size as usize;
        if total <= self.get_index {
            let mut options = ClientOptions::parse(&self.uri).await?;
            options.max_pool_size = Some(self.every_size + 1);
            options.min_pool_size = Some(self.every_size / 100 + 1);
            let client = Arc::new(Client::with_options(options).unwrap());
            self.clients.push(client);
        }

        let block_index = self.get_index / self.every_size as usize;
        let client = Arc::clone(&self.clients[block_index]);

        self.get_index = self.get_index + 1;

        Ok(client)
    }

    async fn shutdown(self) {
        for client in self.clients {
            Arc::try_unwrap(client).unwrap().shutdown().await;
        }
    }
}