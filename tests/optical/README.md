# Two-session CD-ROM fixture

The source ISO 9660 image has seven files in its first session. Its second session removes their directory and adds an empty `Current` directory. The originals are the JPEG, PNG, TIFF, FLAC, RTF, XML and ZIP files under `tests/corpus/files`. Both compressed tracks contain the same 2048-byte logical sectors, encoded separately with cdrdao at commit `7a57ad32993cda11a0fa55c97d5b81e82b508b93`. The fixture generator uses the reference encoder only to prepare test data.

To create an equivalent fixture, use an empty working directory, set `repo` to the Rescue checkout, and provide a checkout of the above cdrdao revision at `$cdrdao`. The creation time can change the ISO and track hashes.

```bash
mkdir originals include
cp "$repo"/tests/corpus/files/sample.{png,jpg,tif,flac,zip,rtf,xml} originals/
xorriso -outdev stdio:optical.iso -volid RECOVERY -map originals /Deleted -commit -end
xorriso -dev stdio:optical.iso -rm_r /Deleted -- -mkdir /Current -- -commit -end
xorriso -indev stdio:optical.iso -ls /
xorriso -load session 1 -indev stdio:optical.iso -osirrox on -extract /Deleted independently-extracted
diff -rq originals independently-extracted
touch include/config.h
c++ -O2 -Iinclude -I"$cdrdao/trackdb" "$repo/tests/optical/encode.cc" "$cdrdao/trackdb/lec.cc" -o encode
./encode 1 <optical.iso >mode1.bin
./encode 2 <optical.iso >xa-form1.bin
```

The portable test uses the supplied tracks, with and without interleaved 96-byte subchannels. Linux additionally extracts the ISO with bchunk and reopens both directory trees with xorriso. Every recovered file is compared to the original bytes and SHA-256.
