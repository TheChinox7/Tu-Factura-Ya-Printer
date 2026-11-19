use crate::{
    api::{self, Client, Job, Report, Station},
    printer,
    settings::{self, Credential, Settings},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

const IDLE: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(45);

pub struct Status {
    pub message: String,
    pub automatic_printing: bool,
    settings_revision: u64,
}

impl Status {
    pub fn inactive(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            automatic_printing: false,
            settings_revision: settings::revision(),
        }
    }

    /// Whether the desktop agent is ready to receive POS jobs for the saved settings.
    /// Browser linkage and the policy for legacy stations are managed by the POS.
    pub fn automatic_printing(&self) -> bool {
        self.automatic_printing && self.settings_revision == settings::revision()
    }
}

pub fn spawn() -> (Arc<Mutex<Status>>, mpsc::SyncSender<()>) {
    let status = Arc::new(Mutex::new(Status::inactive("Iniciando el agente…")));
    let worker_status = Arc::clone(&status);
    // Only one early queue check can be pending while the worker is busy.
    let (wake, wakeups) = mpsc::sync_channel(1);
    let result = thread::Builder::new()
        .name("printing-agent".into())
        .spawn(move || run(&worker_status, &wakeups));
    if result.is_err() {
        update(&status, "No se pudo iniciar el agente de impresión.", false);
    }
    (status, wake)
}

struct Session {
    config: Settings,
    client: Client,
    credential: Credential,
    station: Option<Station>,
}

impl Session {
    fn open() -> Result<Self, String> {
        let config = settings::load()?;
        if config.printer.is_empty() {
            return Err("Selecciona una impresora y guarda los ajustes.".into());
        }
        let client = Client::new(&config.url)?;
        let credential = settings::get_credential(client.origin())?
            .ok_or("Ingresa la clave del sistema y guarda los ajustes.")?;
        Ok(Self {
            config,
            client,
            credential,
            station: None,
        })
    }
}

struct Journal(Connection);

struct Entry {
    job_id: String,
    report: Report,
}

impl Journal {
    fn open() -> Result<Self, String> {
        let path = settings::data_dir()?.join("printing.sqlite3");
        Self::prepare(Connection::open(path).map_err(journal_error)?).map_err(journal_error)
    }

    fn prepare(connection: Connection) -> rusqlite::Result<Self> {
        connection.execute_batch(
            "PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS pending_reports (
                origin TEXT NOT NULL,
                station_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                attempt_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('sent','failed','uncertain')),
                os_job_id TEXT,
                error TEXT,
                PRIMARY KEY (origin, station_id, job_id)
            );",
        )?;
        Ok(Self(connection))
    }

    fn next(&self, origin: &str, station: &str) -> rusqlite::Result<Option<Entry>> {
        self.0
            .query_row(
                "SELECT job_id, attempt_id, status, os_job_id, error FROM pending_reports
             WHERE origin = ? AND station_id = ? ORDER BY rowid LIMIT 1",
                params![origin, station],
                |row| {
                    let status: String = row.get(2)?;
                    Ok(Entry {
                        job_id: row.get(0)?,
                        report: Report {
                            attempt_id: row.get(1)?,
                            status: match status.as_str() {
                                "sent" => "sent",
                                "failed" => "failed",
                                _ => "uncertain",
                            },
                            os_job_id: row.get(3)?,
                            error: row.get(4)?,
                        },
                    })
                },
            )
            .optional()
    }

    fn record(
        &self,
        origin: &str,
        station: &str,
        job_id: &str,
        report: &Report,
    ) -> rusqlite::Result<()> {
        self.0.execute(
            "INSERT INTO pending_reports (origin, station_id, job_id, attempt_id, status, os_job_id, error)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (origin, station_id, job_id) DO UPDATE SET attempt_id=excluded.attempt_id,
             status=excluded.status, os_job_id=excluded.os_job_id, error=excluded.error",
            params![origin, station, job_id, report.attempt_id, report.status, report.os_job_id, report.error],
        )?;
        Ok(())
    }

    fn discard(&self, origin: &str, station: &str, job_id: &str) -> rusqlite::Result<()> {
        self.0.execute(
            "DELETE FROM pending_reports WHERE origin=? AND station_id=? AND job_id=?",
            params![origin, station, job_id],
        )?;
        Ok(())
    }
}

fn run(status: &Mutex<Status>, wakeups: &mpsc::Receiver<()>) {
    let mut journal = None;
    let mut session = None;
    let mut current_revision = None;
    let mut wait = IDLE;
    loop {
        let revision = settings::revision();
        if current_revision != Some(revision) {
            if let Ok(mut status) = status.lock() {
                status.settings_revision = revision;
                status.automatic_printing = false;
                if current_revision.is_some() {
                    status.message = "Comprobando los ajustes de impresión…".into();
                }
            }
            current_revision = Some(revision);
            session = None;
            wait = IDLE;
        }
        if journal.is_none() {
            match Journal::open() {
                Ok(value) => journal = Some(value),
                Err(error) => {
                    update(status, error, false);
                    sleep(Duration::from_secs(15), revision, None);
                    continue;
                }
            }
        }
        if session.is_none() {
            match Session::open() {
                Ok(value) => session = Some(value),
                Err(error) => {
                    update(status, error, false);
                    sleep(IDLE, revision, None);
                    continue;
                }
            }
        }
        let active = session.as_mut().expect("initialized session");
        let result = tick(
            journal.as_ref().expect("initialized journal"),
            active,
            status,
        );
        match result {
            Ok(delay) => {
                wait = IDLE;
                sleep(delay, revision, Some(wakeups));
            }
            Err(error) => {
                if matches!(error.status, Some(401 | 403)) {
                    active.station = None;
                }
                update(status, error.message, false);
                wait = (wait * 2).min(MAX_BACKOFF);
                sleep(wait, revision, None);
            }
        }
    }
}

fn tick(
    journal: &Journal,
    session: &mut Session,
    status: &Mutex<Status>,
) -> Result<Duration, api::Error> {
    if session.station.is_none() {
        session.station = Some(session.client.connection(session.credential.token())?);
    }
    let station = session.station.as_ref().expect("connected station");
    let origin = session.client.origin();
    if let Some(entry) = journal
        .next(origin, &station.id)
        .map_err(api_journal_error)?
    {
        let result =
            session
                .client
                .report(session.credential.token(), &entry.job_id, &entry.report);
        if let Err(error) = &result
            && !matches!(error.status, Some(404 | 409))
        {
            return result.map(|_| Duration::ZERO);
        }
        journal
            .discard(origin, &station.id, &entry.job_id)
            .map_err(api_journal_error)?;
        if let Err(error) = result {
            update(status, error.message, true);
        } else if entry.report.status == "sent" {
            update(
                status,
                "Ticket enviado a la impresora. La confirmación quedó registrada.",
                true,
            );
        } else if let Some(error) = entry.report.error {
            update(status, error, true);
        }
        return Ok(Duration::ZERO);
    }
    let Some(job) = session.client.claim(session.credential.token())? else {
        update(
            status,
            format!("Conectado · {} · Esperando tickets", station.name),
            true,
        );
        return Ok(IDLE);
    };
    update(status, "Enviando ticket a la impresora…", true);
    deliver(journal, session, station, job)?;
    Ok(Duration::ZERO)
}

fn deliver(
    journal: &Journal,
    session: &Session,
    station: &Station,
    job: Job,
) -> Result<(), api::Error> {
    let origin = session.client.origin();
    let mut report = Report {
        attempt_id: job.attempt_id,
        status: "uncertain",
        os_job_id: None,
        error: Some(
            "El agente se cerró durante el envío. Comprueba la cola antes de reimprimir.".into(),
        ),
    };
    // Persist a conservative result before asking the server to start an irreversible submission.
    journal
        .record(origin, &station.id, &job.id, &report)
        .map_err(api_journal_error)?;
    if let Err(error) =
        session
            .client
            .start(session.credential.token(), &job.id, &report.attempt_id)
    {
        report.status = "failed";
        report.error = Some(
            "El agente no pudo iniciar el envío. El ticket todavía no salió de este equipo.".into(),
        );
        journal
            .record(origin, &station.id, &job.id, &report)
            .map_err(api_journal_error)?;
        return Err(error);
    }
    let spool_name = format!("TFY {}", job.id);
    let result = match &job.receipt {
        api::JobReceipt::Pdf { pdf_base64 } => {
            printer::submit_pdf(&session.config, pdf_base64, &spool_name)
        }
        api::JobReceipt::Text(receipt) => {
            printer::submit(&session.config, receipt, &spool_name, false)
        }
    };
    match result {
        Ok(id) => {
            report.status = "sent";
            report.os_job_id = id;
            report.error = None;
        }
        Err(error) => {
            report.status = if error.uncertain {
                "uncertain"
            } else {
                "failed"
            };
            report.error = Some(error.message);
        }
    }
    journal
        .record(origin, &station.id, &job.id, &report)
        .map_err(api_journal_error)?;
    Ok(())
}

fn journal_error(_error: rusqlite::Error) -> String {
    "No se pudo escribir el registro local de trabajos. Comprueba el espacio y los permisos.".into()
}

fn api_journal_error(error: rusqlite::Error) -> api::Error {
    api::Error {
        status: None,
        message: journal_error(error),
    }
}

fn update(status: &Mutex<Status>, message: impl Into<String>, automatic_printing: bool) {
    if let Ok(mut status) = status.lock() {
        status.message = message.into();
        status.automatic_printing = automatic_printing;
    }
}

fn sleep(delay: Duration, revision: u64, mut wakeups: Option<&mpsc::Receiver<()>>) {
    let until = std::time::Instant::now() + delay;
    while settings::revision() == revision {
        let remaining = until.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let interval = remaining.min(Duration::from_millis(500));
        if let Some(wakeups_receiver) = wakeups {
            match wakeups_receiver.recv_timeout(interval) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => wakeups = None,
            }
        } else {
            thread::sleep(interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_wake_is_consumed_by_the_next_healthy_wait() {
        let (wake, wakeups) = mpsc::sync_channel(1);
        wake.try_send(()).unwrap();

        sleep(Duration::from_millis(1), settings::revision(), None);
        assert_eq!(wake.try_send(()), Err(mpsc::TrySendError::Full(())));

        sleep(IDLE, settings::revision(), Some(&wakeups));
        assert_eq!(wakeups.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn wake_burst_collapses_into_one_pending_queue_check() {
        let (wake, wakeups) = mpsc::sync_channel(1);
        wake.try_send(()).unwrap();
        for _ in 0..20 {
            assert_eq!(wake.try_send(()), Err(mpsc::TrySendError::Full(())));
        }

        assert_eq!(wakeups.try_recv(), Ok(()));
        assert_eq!(wakeups.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn pending_ack_survives_close_without_reprinting() {
        let path =
            std::env::temp_dir().join(format!("tfy-printer-test-{}.sqlite", std::process::id()));
        let journal = Journal::prepare(Connection::open(&path).unwrap()).unwrap();
        journal
            .record(
                "https://one.example",
                "station-a",
                "job-a",
                &Report {
                    attempt_id: "attempt-a".into(),
                    status: "sent",
                    os_job_id: Some("device-12".into()),
                    error: None,
                },
            )
            .unwrap();
        drop(journal);
        let journal = Journal::prepare(Connection::open(&path).unwrap()).unwrap();
        assert!(
            journal
                .next("https://two.example", "station-a")
                .unwrap()
                .is_none()
        );
        assert!(
            journal
                .next("https://one.example", "station-b")
                .unwrap()
                .is_none()
        );
        let pending = journal
            .next("https://one.example", "station-a")
            .unwrap()
            .unwrap();
        assert_eq!(pending.job_id, "job-a");
        assert_eq!(pending.report.status, "sent");
        assert_eq!(pending.report.os_job_id.as_deref(), Some("device-12"));
        journal
            .discard("https://one.example", "station-a", "job-a")
            .unwrap();
        drop(journal);
        std::fs::remove_file(path).unwrap();
    }
}
