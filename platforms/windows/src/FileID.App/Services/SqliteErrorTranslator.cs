// Turns raw SQLite and IO exceptions into actionable user-facing sentences.

using System;

namespace FileID.Services;

public static class SqliteErrorTranslator
{
    public static string Humanize(Exception ex)
    {
        if (ex is Microsoft.Data.Sqlite.SqliteException se)
        {
            return se.SqliteErrorCode switch
            {
                11 => "The library database is corrupted. Re-run a scan to rebuild it, or restart FileID.", // SQLITE_CORRUPT
                26 => "That file isn't a valid FileID library database.",                                   // SQLITE_NOTADB
                5 or 6 => "The library is busy with another operation. Try again in a moment.",              // BUSY / LOCKED
                14 => "FileID can't open the library database — check the folder path and permissions, then re-pick your folder.", // CANTOPEN
                13 => "Your disk is full. Free up some space and try again.",                                // FULL
                10 => "A disk read error occurred reading the library. Try again, or restart FileID.",       // IOERR
                _ => "FileID couldn't read the library database. Try restarting the app or re-running a scan.",
            };
        }
        if (ex is System.IO.IOException)
        {
            return "FileID couldn't read the library (a disk or network issue). Check the drive is connected and try again.";
        }
        if (ex is UnauthorizedAccessException)
        {
            return "FileID doesn't have permission to read the library. Check the folder's permissions, then try again.";
        }
        return "Something went wrong reading the library. Try restarting FileID.";
    }
}
