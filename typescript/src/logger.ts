// Structured logger — §13.1 of the spec
import * as fs from "node:fs";
import * as path from "node:path";

export type LogLevel = "debug" | "info" | "warn" | "error";

interface LogEntry {
  level: LogLevel;
  time: string;
  msg: string;
  [key: string]: unknown;
}

class Logger {
  private fileStream: fs.WriteStream | null = null;

  initFileLog(logsRoot: string): void {
    try {
      fs.mkdirSync(logsRoot, { recursive: true });
      const logFile = path.join(logsRoot, "symphony.log");
      this.fileStream = fs.createWriteStream(logFile, { flags: "a" });
    } catch (err) {
      process.stderr.write(`[warn] Failed to open log file: ${err}\n`);
    }
  }

  private emit(level: LogLevel, msg: string, fields?: Record<string, unknown>): void {
    const entry: LogEntry = {
      level,
      time: new Date().toISOString(),
      msg,
      ...fields,
    };
    const line = JSON.stringify(entry);

    // Always write to stderr for operator visibility
    process.stderr.write(line + "\n");

    // Also write to file sink if configured
    if (this.fileStream) {
      this.fileStream.write(line + "\n");
    }
  }

  debug(msg: string, fields?: Record<string, unknown>): void {
    this.emit("debug", msg, fields);
  }

  info(msg: string, fields?: Record<string, unknown>): void {
    this.emit("info", msg, fields);
  }

  warn(msg: string, fields?: Record<string, unknown>): void {
    this.emit("warn", msg, fields);
  }

  error(msg: string, fields?: Record<string, unknown>): void {
    this.emit("error", msg, fields);
  }
}

export const logger = new Logger();
