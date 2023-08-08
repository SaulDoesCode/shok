cd ./static
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/static/space.html
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/static/ks.css
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/static/domlib.js
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/static/icon.svg
cd ../src/
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/src/main.rs
cd ../
curl -JLO https://raw.githack.com/SaulDoesCode/shok/master/Cargo.toml
nohup cargo build --release &
