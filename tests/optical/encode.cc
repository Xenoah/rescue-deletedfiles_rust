#include <array>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include "lec.h"
int main(int argc,char **argv) {
    if(argc!=2 || (std::strcmp(argv[1],"1") && std::strcmp(argv[1],"2"))) return 1;
    bool xa=argv[1][0]=='2';
    std::array<unsigned char,2352> sector;
    for(unsigned frame=150;; ++frame) {
        sector.fill(0);
        auto n=std::fread(sector.data()+(xa?24:16),1,2048,stdin);
        if(n==0) return std::ferror(stdin)?1:0;
        if(n!=2048) return 2;
        if(xa) {
            sector[18]=sector[22]=8;
            lec_encode_mode2_form1_sector(frame,sector.data());
        } else lec_encode_mode1_sector(frame,sector.data());
        if(std::fwrite(sector.data(),1,sector.size(),stdout)!=sector.size()) return 3;
    }
}
