// Independent OpenEXR SDK reader for the bounded, zero-origin multiresolution fixtures.
#include <OpenEXR/ImfChannelList.h>
#include <OpenEXR/ImfFrameBuffer.h>
#include <OpenEXR/ImfHeader.h>
#include <OpenEXR/ImfTiledInputFile.h>
#include <cstdint>
#include <iostream>
#include <vector>

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    try {
        Imf::TiledInputFile file(argv[1]);
        const auto& channels = file.header().channels();
        for (int y = 0; y < file.numYLevels(); ++y) {
            for (int x = 0; x < file.numXLevels(); ++x) {
                if (!file.isValidLevel(x, y)) continue;
                const auto box = file.dataWindowForLevel(x, y);
                if (box.min.x != 0 || box.min.y != 0) return 3;
                const auto width = file.levelWidth(x);
                const auto height = file.levelHeight(y);
                if (width <= 0 || height <= 0 || width > 1024 || height > 1024) return 3;
                for (auto it = channels.begin(); it != channels.end(); ++it) {
                    const auto type = it.channel().type;
                    const size_t stride = type == Imf::HALF ? 2 : 4;
                    std::vector<char> bytes(size_t(width) * size_t(height) * stride);
                    Imf::FrameBuffer frame;
                    frame.insert(it.name(), Imf::Slice(type, bytes.data(), stride, stride * width));
                    file.setFrameBuffer(frame);
                    file.readTiles(0, file.numXTiles(x) - 1, 0, file.numYTiles(y) - 1, x, y);
                    std::cout << x << ' ' << y << ' ' << width << ' ' << height << ' '
                              << static_cast<int>(type) << ' ' << it.name() << ' ' << bytes.size() << '\n';
                    std::cout.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
                }
            }
        }
        return std::cout ? 0 : 4;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
