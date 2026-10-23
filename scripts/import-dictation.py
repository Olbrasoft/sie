#!/usr/bin/env python3
"""One-time, read-only-source import into an independent local SIE database."""
import argparse
import getpass
import json
import os
from pathlib import Path

import psycopg2
from psycopg2 import sql
from psycopg2.extras import Json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-db', default='virtual_assistant')
    parser.add_argument('--target-db', default='sie')
    parser.add_argument('--filters', type=Path, required=True)
    parser.add_argument('--write-config', action='store_true')
    parser.add_argument('--coordinate-virtual-assistant', action='store_true')
    args = parser.parse_args()
    if args.source_db == args.target_db or args.target_db == 'virtual_assistant':
        raise SystemExit('Source and target must be independent databases')
    connection_args = dict(host='/var/run/postgresql', user=getpass.getuser())
    source = psycopg2.connect(dbname=args.source_db, **connection_args)
    source.set_session(readonly=True)
    columns = 'id, incorrect_text, correct_text, case_sensitive, priority, is_active, notes, created_at, updated_at'
    with source.cursor() as cursor:
        cursor.execute(f'SELECT {columns} FROM transcription_corrections ORDER BY id')
        rules = cursor.fetchall()
    filters = json.loads(args.filters.read_text())
    admin = psycopg2.connect(dbname='postgres', **connection_args)
    admin.autocommit = True
    with admin.cursor() as cursor:
        cursor.execute('SELECT 1 FROM pg_database WHERE datname=%s', (args.target_db,))
        if cursor.fetchone():
            raise SystemExit('Target database already exists; refusing to overwrite it')
        cursor.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(args.target_db)))
    target = psycopg2.connect(dbname=args.target_db, **connection_args)
    with target, target.cursor() as cursor:
        cursor.execute((Path(__file__).resolve().parent.parent / 'migrations/001_dictation.sql').read_text())
        cursor.executemany(f'INSERT INTO transcription_corrections ({columns}) VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s)', rules)
        cursor.execute("SELECT setval(pg_get_serial_sequence('transcription_corrections','id'), COALESCE(MAX(id),1), MAX(id) IS NOT NULL) FROM transcription_corrections")
        cursor.execute('UPDATE postprocessing_settings SET filters=%s WHERE id=TRUE', (Json(filters),))
    with source.cursor() as cursor:
        cursor.execute(f'SELECT {columns} FROM transcription_corrections ORDER BY id')
        assert cursor.fetchall() == rules, 'Source data changed during import'
    print(f'Copied {len(rules)} rules ({sum(row[5] for row in rules)} active); source data unchanged.')
    if args.write_config:
        path = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home()/'.config'))) / 'sie/config.toml'
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if path.exists():
            raise SystemExit('Database imported, but existing configuration was not overwritten')
        model = Path.home()/'.local/share/whisper-models/ggml-large-v3-turbo.bin'
        db = f'host=/var/run/postgresql dbname={args.target_db} user={getpass.getuser()}'
        text = '[dictation]\nenabled = true\nmodel_path = '+json.dumps(str(model))+'\nlanguage = "cs"\ndatabase_url = '+json.dumps(db)+'\nuse_gpu = true\nmax_duration_seconds = 300\n'
        if args.coordinate_virtual_assistant:
            text += 'virtual_assistant_speech_lock = true\n'
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as output:
            output.write(text)
        print('Created local SIE dictation configuration without a database password.')
    target.close()
    admin.close()
    source.close()


if __name__ == '__main__':
    main()
