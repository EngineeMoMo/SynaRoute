#[path = "../../src/laya_artifacts.rs"]
mod artifacts;
#[path = "../../src/http_ingress.rs"]
#[allow(dead_code)]
mod http_ingress;
#[path = "../../src/laya_question.rs"]
mod question;
mod service;

fn main() -> std::process::ExitCode {
    match service::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("SYNAROUTE_LAYA_ERROR:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
