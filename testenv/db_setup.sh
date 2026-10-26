#!/bin/sh
# Local loopback MariaDB/MySQL + PostgreSQL for tests/db_live.rs (throwaway credentials sx/sx).
# Debian/Ubuntu: sudo apt-get install mariadb-server postgresql
set -e
sudo service mariadb start 2>/dev/null || sudo service mysql start
sudo service postgresql start
sudo mysql -e "CREATE USER IF NOT EXISTS 'sx'@'%' IDENTIFIED BY 'sx'; GRANT ALL ON *.* TO 'sx'@'%'; CREATE DATABASE IF NOT EXISTS sxtest;"
sudo -u postgres psql -qc "DO \$\$BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sx') THEN CREATE ROLE sx LOGIN SUPERUSER PASSWORD 'sx'; END IF; END\$\$;"
sudo -u postgres psql -lqt | cut -d'|' -f1 | grep -qw sxtest || sudo -u postgres createdb -O sx sxtest
echo "export SHELLAX_TEST_MYSQL_URL=mysql://sx:sx@127.0.0.1:3306/sxtest SHELLAX_TEST_PG_URL=postgres://sx:sx@127.0.0.1:5432/sxtest"
echo "then: cargo test --test db_live -- --ignored --test-threads=1"
