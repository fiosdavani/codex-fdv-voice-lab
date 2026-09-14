#include "windows_audio.hpp"
#include <iostream>
#include <string>
int main(int argc, char** argv) {
    if (argc == 2 && std::string(argv[1]) == "--capabilities") {
        std::cout << vai::native_capabilities() << '\n';
        return 0;
    }
    std::cerr << "Only --capabilities is exposed. Native audio execution is not enabled.\n";
    return 2;
}
