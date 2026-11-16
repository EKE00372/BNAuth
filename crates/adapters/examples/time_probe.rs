fn main() {
    let started = std::time::Instant::now();
    match bnauth_adapters::time_sync::synchronize(None) {
        Ok(sample) => println!(
            "OK elapsed_ms={} rtt_ms={} offset_ms={}",
            started.elapsed().as_millis(),
            sample.record.rtt_ms,
            sample.record.offset_ms
        ),
        Err(error) => {
            eprintln!(
                "FAIL elapsed_ms={} reason={error}",
                started.elapsed().as_millis()
            );
            std::process::exit(1);
        }
    }
}
