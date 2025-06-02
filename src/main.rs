use std::{collections::HashMap, fs::Permissions, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};

use anyhow::{Context as _, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64ct::Encoding;
use clap::Parser;
use md5::{Digest as _, Md5};
use serde::Deserialize;
use subprocess::{Popen, PopenConfig};
use tokio::sync::{
    RwLock,
    broadcast::{self, Sender},
};
use uuid::Uuid;

#[derive(Copy, Clone)]
enum RunStatus {
    Running,
    Success,
    Fail,
}

#[derive(Clone)]
struct AppState {
    runs: Arc<RwLock<HashMap<String, RunStatus>>>,

    run_status_sender: Arc<Sender<()>>,
}

#[derive(Deserialize)]
struct RunRequest {
    workdir: String,
    cmd: Vec<String>,
}

async fn run(State(state): State<AppState>, Json(request): Json<RunRequest>) -> Result<String, AppError> {
    let uuid = Uuid::new_v4().to_string();

    if request.workdir.contains('~') || !request.workdir.starts_with('/') {
        return Err(anyhow::anyhow!("Path math be absolute").into());
    }

    let workdir = PathBuf::from(&request.workdir);
    let _ = std::fs::create_dir_all(&workdir);

    state.runs.write().await.insert(uuid.clone(), RunStatus::Running);
    log::info!("[{uuid}] Started job {:?} at {}", request.cmd, request.workdir);

    let uuid_func = uuid.clone();
    tokio::spawn(async {
        let request = request;
        let state = state;
        let uuid = uuid_func;
        let cmd = request.cmd.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut p = match Popen::create(
                &cmd,
                PopenConfig {
                    cwd: Some(workdir.into()),
                    ..Default::default()
                },
            ) {
                Ok(x) => x,
                Err(e) => {
                    bail!("Error when starting process: {e:?}");
                }
            };
            p.wait().ok();
            let exit_status = p.poll().with_context(|| "Can't get exit status")?;
            if !exit_status.success() {
                anyhow::bail!("Exit status {:?}", exit_status);
            }

            Ok(())
        })
        .await;

        match res {
            Ok(_) => {
                log::info!("[{uuid}] Succeeded job {:?} at {}", request.cmd, request.workdir,);
                state.runs.write().await.insert(uuid, RunStatus::Success);
            }
            Err(e) => {
                log::error!("[{uuid}] Failed job {:?} at {}: {e:?}", request.cmd, request.workdir,);
                state.runs.write().await.insert(uuid, RunStatus::Fail);
            }
        };
        let _ = state.run_status_sender.send(());
    });

    Ok(uuid)
}

async fn wait_run(State(state): State<AppState>, Path(id): Path<String>) -> Result<&'static str, AppError> {
    let mut ch = state.run_status_sender.subscribe();
    loop {
        let status = state
            .runs
            .read()
            .await
            .get(&id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Run {id} not found"))?;

        match status {
            RunStatus::Success => return Ok("ok"),
            RunStatus::Fail => return Ok("failed"),
            RunStatus::Running => {}
        }
        let _ = ch.recv().await;
    }
}

#[derive(Deserialize)]
struct OfferFilesRequest {
    workdir: String,
    hashes: HashMap<String, String>,
}

async fn offer_files(Json(request): Json<OfferFilesRequest>) -> Json<Vec<String>> {
    let workdir = PathBuf::from(&request.workdir);
    let mut result = Vec::new();
    for (filename, hash) in request.hashes.into_iter() {
        let path = workdir.join(&filename);
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            let my_hash = base16ct::lower::encode_string(&Md5::digest(bytes));
            if hash == my_hash {
                continue;
            }
        }
        result.push(filename);
    }
    Json(result)
}

#[derive(Deserialize)]
struct FileInfo {
    data: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Deserialize)]
struct SendFilesRequest {
    workdir: String,
    files: HashMap<String, FileInfo>,
}

async fn send_files(Json(request): Json<SendFilesRequest>) {
    let workdir = PathBuf::from(&request.workdir);
    for (filename, info) in request.files.into_iter() {
        let path = workdir.join(&filename);
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let data = base64ct::Base64::decode_vec(&info.data).unwrap();
        std::fs::write(&path, data).unwrap();
        if info.executable {
            std::fs::set_permissions(path, Permissions::from_mode(0o777)).unwrap();
        }
    }
}

#[derive(Deserialize)]
struct GetFileRequest {
    workdir: String,
    path: String,
}

async fn get_file(Json(request): Json<GetFileRequest>) -> String {
    let workdir = PathBuf::from(&request.workdir);
    let path = workdir.join(request.path);
    let bytes = std::fs::read(&path).unwrap();
    base64ct::Base64::encode_string(&bytes)
}

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    env_logger::init();

    let state = AppState {
        runs: Arc::new(RwLock::new(HashMap::default())),
        run_status_sender: Arc::new(broadcast::channel(1).0),
    };
    let app = Router::<AppState>::new()
        .route("/ping", get(|| async { "pong" }))
        .route("/run", post(run))
        .route("/wait-run/{id}", get(wait_run))
        .route("/offer-files", post(offer_files))
        .route("/send-files", post(send_files))
        .route("/get-file", post(get_file))
        .with_state(state)
        .layer(DefaultBodyLimit::max(1usize << 30));

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", args.port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response<Body> {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
