use std::io::{Read, Write};
use std::sync::mpsc;

use anyhow::Result;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const READ_BUF_SIZE: usize = 8192;

pub struct PtyBridge {
    writer: Box<dyn Write + Send>,
    resize_handle: Box<dyn MasterPty + Send>,
}

impl PtyBridge {
    pub fn spawn(
        args: &[String],
        envs: &[(String, String)],
    ) -> Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("no shell specified"));
        }
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(size)?;
        let mut cmd = CommandBuilder::new(&args[0]);
        if args.len() > 1 {
            cmd.args(&args[1..]);
        }
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let _child = pair.slave.spawn_command(cmd)?;

        let writer = pair.master.take_writer()?;
        let reader = pair.master.try_clone_reader()?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_BUF_SIZE];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((
            Self {
                writer,
                resize_handle: pair.master,
            },
            rx,
        ))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.resize_handle.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_empty_args() {
        let result = PtyBridge::spawn(&[], &[]);
        assert!(result.is_err());
    }
}
