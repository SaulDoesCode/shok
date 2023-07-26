# start the server (./target/release/shok) as a daemon and redirect the output to a log file
# the log file is rotated every day
# the server is restarted every day at 4:00 AM
# the server is restarted every time the binary is updated
# the server is restarted every time the system is rebooted
# the server is restarted every time the network is reconnected

# kill the shok process if it running
SHOK_PATH=./target/release/shok

pkill shok
sleep 3
nohup $SHOK_PATH &
