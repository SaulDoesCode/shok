# start the server (./target/release/shok) as a daemon and redirect the output to a log file
# the log file is rotated every day
# kill the shok process if it running

pkill shok
sleep 3
mv nohup.out ../$(date +%Y-%m-%d).log
nohup cargo run --release &
