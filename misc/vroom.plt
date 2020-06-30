#!/usr/bin/gnuplot

# set term wxt size 1920,1080
set terminal pngcairo size 720,405 enhanced font 'Verdana,10'
set output 'vroom.png'
set title "Insertion time"
set xlabel "Index"
set ylabel "Latency [ms]"
set xrange [0:2e6]

set style line 1 lc rgb '#1b9e77'
set style line 2 lc rgb '#d95f02'
set style line 3 lc rgb '#7570b3'

set key left top
set key box linestyle 3

plot "< grep griddle vroom.dat" u 1:3 t "griddle::HashMap" ls 2 lw 2 ps 1.5, \
     "< grep hashbrown vroom.dat" u 1:3 t "hashbrown::HashMap" ls 1 lw 2 ps 1.5
