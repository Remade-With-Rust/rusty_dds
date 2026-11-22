#!/bin/bash
set -e
CP=/root/astcws/cp; A=/root/astcws
mkdir -p $CP/fix
K=$A/thirdparty/KTX-Software/tests/resources/ktx2
cp "$A/thirdparty/image-dds/test-data/images/astc/ASTC 6x5.dds" $CP/fix/ASTC_6x5.dds
ENC=$(find $A/astc-encoder/build $A/rel571 -name 'astcenc-avx2' -type f 2>/dev/null | head -1)
echo enc=$ENC
$ENC -cs $A/kodak/kodim05.png $CP/fix/m6x6.ktx 6x6 -fast -silent
cp $K/ktx_app_astc_8x8.ktx2 $CP/fix/app8x8_zstd.ktx2
$A/ktxsw/KTX-Software-4.4.2-Linux-x86_64/bin/ktxsc --zcmp 10 $CP/fix/app8x8_zstd.ktx2 || LD_LIBRARY_PATH=$A/ktxsw/KTX-Software-4.4.2-Linux-x86_64/lib $A/ktxsw/KTX-Software-4.4.2-Linux-x86_64/bin/ktxsc --zcmp 10 $CP/fix/app8x8_zstd.ktx2
ls -la $CP/fix
