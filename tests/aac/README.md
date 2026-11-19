# AAC-LC in ADTS

FFmpeg encoded seven original sine-wave recordings as unprotected ADTS with one AAC-LC block per frame. The manifest records each frequency, sample rate, channel count, bit rate, frame count and SHA-256. Durations were 0.24 seconds before encoder delay and padding. The command used FFmpeg's sine source, the native AAC encoder with the aac_low profile, and its ADTS muxer.

The portable test compares the original bytes and SHA-256 with both normal and indexed carve recovery. The native Linux test uses e2fsprogs to write and unlink the files in a disposable ext4 image, checks each deleted name with fls, and independently compares decoded PCM from FFmpeg for every recovered file.
