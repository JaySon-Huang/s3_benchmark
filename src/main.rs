use clap::Parser;
use futures::executor::block_on;
use rand::prelude::*;
use rusoto_core::{Region, RusotoError, credential::DefaultCredentialsProvider};
use rusoto_s3::{GetObjectRequest, ListObjectsV2Request, PutObjectRequest, S3Client, S3};
use std::{str::FromStr, sync::Arc, time::Instant};
use tokio::io::AsyncReadExt;

#[derive(Debug)]
enum RequestType {
    Put,
    Get,
}

#[derive(Debug)]
struct Stats {
    start_time: Instant,
    end_time: Instant,
    request_type: RequestType,
    file_size: usize,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    endpoint: String,

    #[arg(short, long)]
    bucket: String,

    #[arg(short, long)]
    root_prefix: String,

    #[arg(long, default_value_t = 1)]
    put_concurrency: u32,

    #[arg(long, default_value_t = 1)]
    put_count_per_thread: u32,

    #[arg(long, default_value_t = 1)]
    get_concurrency: u32,

    #[arg(long, default_value_t = 1)]
    get_count_per_thread: u32,

    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let endpoint = args.endpoint;
    let bucket = args.bucket;
    let root_prefix = args.root_prefix;
    let put_concurrency = args.put_concurrency;
    let put_count_per_thread = args.put_count_per_thread;
    let get_concurrency = args.get_concurrency;
    let get_count_per_thread = args.get_count_per_thread;

    let verbose = args.verbose;

    let region = Region::from_str(endpoint.as_str()).unwrap();
    let credentials = DefaultCredentialsProvider::new().unwrap();
    let s3 = S3Client::new_with(
        rusoto_core::request::HttpClient::new().unwrap(),
        credentials,
        region,
    );
    // let s3 = S3Client::new(Region::from_str(endpoint.as_str()).unwrap());

    let stats_vec = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut tasks_future = Vec::new();

    // spawn put threads
    for _ in 0..put_concurrency {
        let s3 = s3.clone();
        let stats_vec = Arc::clone(&stats_vec);
        let bucket = bucket.clone();
        let root_prefix = root_prefix.clone();
        let put_task_future = tokio::task::spawn(async move {
            for _ in 0..put_count_per_thread {
                let file_size = thread_rng().gen_range(1024..1024 * 1024 * 100);
                let file_name = format!("put_{}", file_size);
                let key = format!("{}/{}", root_prefix, file_name);
                let body: Vec<u8> = (0..file_size).map(|_| thread_rng().gen()).collect();
                let start_time = Instant::now();
                let put_req = PutObjectRequest {
                    bucket: bucket.clone(),
                    key: key.clone(),
                    body: Some(body.into()),
                    ..Default::default()
                };
                match s3.put_object(put_req).await {
                    Ok(_) => {
                        let end_time = Instant::now();
                        let stats = Stats {
                            start_time,
                            end_time,
                            request_type: RequestType::Put,
                            file_size,
                        };
                        if verbose {
                            println!(
                                "put key {} takes {}ms",
                                key,
                                end_time.duration_since(start_time).as_millis()
                            );
                        }
                        stats_vec.lock().unwrap().push(stats);
                    }
                    Err(RusotoError::HttpDispatch(_)) => {
                        println!("HttpDispatch");
                    }
                    Err(e) => {
                        eprintln!("Error putting object: {:?}", e);
                    }
                }
            }
        });
        tasks_future.push(put_task_future);
    }

    // spawn get threads
    for _ in 0..get_concurrency {
        let s3 = s3.clone();
        let stats_vec = Arc::clone(&stats_vec);
        let bucket = bucket.clone();
        let root_prefix = root_prefix.clone();

        let get_task_future = tokio::task::spawn(async move {
            let mut get_num = 0;
            loop {
                if get_num >= get_count_per_thread {
                    break;
                }
                let mut request = ListObjectsV2Request {
                    bucket: bucket.clone(),
                    prefix: Some(root_prefix.clone()),
                    ..Default::default()
                };
                let mut objects = Vec::new();
                loop {
                    match s3.list_objects_v2(request.clone()).await {
                        Ok(result) => {
                            if result.contents.is_none() {
                                break;
                            }
                            objects.extend(result.contents.unwrap());
                            if result.next_continuation_token.is_none() {
                                break;
                            }
                            request.continuation_token = result.next_continuation_token;
                        }
                        Err(RusotoError::HttpDispatch(_)) => {
                            println!("HttpDispatch");
                        }
                        Err(e) => {
                            eprintln!("Error listing objects: {:?}", e);
                            break;
                        }
                    }
                }
                if objects.len() == 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                get_num += 1;
                let key = objects[thread_rng().gen_range(0..objects.len())]
                    .key
                    .clone()
                    .unwrap();
                let start_time = Instant::now();
                let get_req = GetObjectRequest {
                    bucket: bucket.clone(),
                    key: key.clone(),
                    ..Default::default()
                };
                match s3.get_object(get_req).await {
                    Ok(resp) => match resp.body {
                        Some(body) => {
                            let mut body = body.into_async_read();
                            let mut buf = Vec::new();
                            body.read_to_end(&mut buf).await.unwrap();
                            let end_time = Instant::now();
                            let stats = Stats {
                                start_time,
                                end_time,
                                request_type: RequestType::Get,
                                file_size: buf.len(),
                            };
                            stats_vec.lock().unwrap().push(stats);
                            if verbose {
                                println!(
                                    "get key {} takes {}ms",
                                    key,
                                    end_time.duration_since(start_time).as_millis()
                                );
                            }
                        }
                        None => {
                            eprintln!("No body in response");
                        }
                    },
                    Err(RusotoError::HttpDispatch(_)) => {}
                    Err(e) => {
                        eprintln!("Error getting object: {:?}", e);
                    }
                }
            }
        });
        tasks_future.push(get_task_future);
    }

    let _results = block_on(futures::future::join_all(tasks_future));

    let mut put_count = 0;
    let mut get_count = 0;
    let mut put_time = 0;
    let mut get_time = 0;
    let mut put_file_size = 0;
    let mut get_file_size = 0;
    let stat_vec = stats_vec.lock().unwrap();
    for i in stat_vec.iter() {
        match i.request_type {
            RequestType::Put => {
                put_count += 1;
                put_time += i.end_time.duration_since(i.start_time).as_millis() as u128;
                put_file_size += i.file_size;
            }
            RequestType::Get => {
                get_count += 1;
                get_time += i.end_time.duration_since(i.start_time).as_millis() as u128;
                get_file_size += i.file_size;
            }
        }
    }
    let put_avg_time = put_time / put_count as u128;
    let get_avg_time = get_time / get_count as u128;

    println!(
        "PUT stats: count={}, total_time={}ms, avg_time={}ms, total_size={} MB",
        put_count,
        put_time,
        put_avg_time,
        put_file_size / 1024 / 1024
    );
    println!(
        "GET stats: count={}, total_time={}ms, avg_time={}ms, total_size={} MB",
        get_count,
        get_time,
        get_avg_time,
        get_file_size / 1024 / 1024
    );
}
