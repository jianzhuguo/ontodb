@echo off
cd /d E:\ontodb
E:\ontodb\target\release\ontodb-server.exe --data-dir E:\sembio\data\ontodb_clean --http 127.0.0.1:7912 --memtable-size 268435456 --compression-level 1 --no-sync-wal --index-storage-mode hybrid --index-hybrid-threshold 100000 >> E:\ontodb\server_out.log 2>> E:\ontodb\server_err.log
