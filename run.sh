# start the server (./target/release/shok) as a daemon and redirect the output to a log file
# the log file is rotated every day
# kill the shok process if it running

# generate a random port number
no_over_ride=$(shuf -i 2000-65000 -n 1)

pkill shok
sleep 3
mv nohup.out ../$(date +%Y-%m-%d)-$no_over_ride.log
nohup cargo run --release &
