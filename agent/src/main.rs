use std::process::ExitCode;

fn main() -> ExitCode {
    let args = match phoned::Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("phoned: {e}\n{}", phoned::USAGE);
            return ExitCode::from(2);
        }
    };
    // Tek iş parçacıklı çalışma zamanı (ADR-01): iş az, kilit yarışı yok, RSS küçük.
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("phoned: çalışma zamanı kurulamadı: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(phoned::serve(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("phoned: {e}");
            ExitCode::FAILURE
        }
    }
}
