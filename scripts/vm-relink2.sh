#!/bin/bash
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 > /home/aether/relink2.log 2>&1
echo "make exit: $?"
grep -E "Error [0-9]|error:" /home/aether/relink2.log | head -4
