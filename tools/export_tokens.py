#!/usr/bin/env python3
import sqlite3
import sys
import argparse
import os


def export_tokens(db_path):
    if not os.path.exists(db_path):
        print(f"Error: Database file '{db_path}' not found.", file=sys.stderr)
        sys.exit(1)

    try:
        # Open in read-only mode just in case
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        cursor = conn.cursor()

        # The Rust implementation uses id=1 for the single token row
        cursor.execute("SELECT access_token, refresh_token FROM tokens WHERE id = 1")
        row = cursor.fetchone()

        if row:
            access_token, refresh_token = row
            print(f"OPEN115_ACCESS_TOKEN={access_token}")
            print(f"OPEN115_REFRESH_TOKEN={refresh_token}")
        else:
            print(
                f"Error: No tokens found in database '{db_path}' (expected row with id=1).",
                file=sys.stderr,
            )
            sys.exit(1)

    except sqlite3.Error as e:
        print(f"Database error: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        if "conn" in locals() and conn:
            conn.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Export 115 tokens from SQLite cache to stdout in .env format"
    )
    parser.add_argument(
        "db_path",
        nargs="?",
        default="cache-115.db",
        help="Path to SQLite database (default: cache-115.db)",
    )
    args = parser.parse_args()

    export_tokens(args.db_path)
