// crn2blocks: decompress a CRN (Unity crunch variant) to raw texture blocks.
// One-time converter built by build.sh against Unity's open-source crunch fork;
// the game's own files are not rebuilt, this only reads a public codec format.
// Usage: crn2blocks <in.crn> <out.bin> [mip]
#include <stdio.h>
#include <stdlib.h>
#include <vector>

extern "C" {
unsigned int crn_get_width(void *src, unsigned int src_size);
unsigned int crn_get_height(void *src, unsigned int src_size);
unsigned int crn_get_levels(void *src, unsigned int src_size);
unsigned int crn_get_dxt_format(void *src, unsigned int src_size);
unsigned int crn_get_bytes_per_block(void *src, unsigned int src_size);
unsigned int crn_get_uncompressed_size(void *src, unsigned int src_size, unsigned int level);
void crn_decompress(void *src, unsigned int src_size, void *dst, unsigned int dst_size,
                    unsigned int firstLevel, unsigned int levelCount);
}

int main(int argc, char **argv) {
  if (argc < 3) {
    fprintf(stderr, "usage: crn2blocks <in.crn> <out.bin> [mip]\n");
    return 2;
  }
  FILE *f = fopen(argv[1], "rb");
  if (!f) { perror("open in"); return 1; }
  fseek(f, 0, SEEK_END);
  long n = ftell(f);
  fseek(f, 0, SEEK_SET);
  std::vector<unsigned char> buf((size_t)n);
  if (fread(buf.data(), 1, (size_t)n, f) != (size_t)n) { fclose(f); return 1; }
  fclose(f);

  unsigned int mip = (argc > 3) ? (unsigned int)atoi(argv[3]) : 0;
  unsigned int w = crn_get_width(buf.data(), (unsigned int)n) >> mip;
  unsigned int h = crn_get_height(buf.data(), (unsigned int)n) >> mip;
  unsigned int levels = crn_get_levels(buf.data(), (unsigned int)n);
  unsigned int fmt = crn_get_dxt_format(buf.data(), (unsigned int)n);
  unsigned int bpb = crn_get_bytes_per_block(buf.data(), (unsigned int)n);
  unsigned int size = crn_get_uncompressed_size(buf.data(), (unsigned int)n, mip);
  if (size == 0 || w == 0 || h == 0) {
    fprintf(stderr, "bad texture info\n");
    return 1;
  }
  std::vector<unsigned char> out(size);
  crn_decompress(buf.data(), (unsigned int)n, out.data(), size, mip, 1);
  FILE *o = fopen(argv[2], "wb");
  if (!o) { perror("open out"); return 1; }
  fwrite(out.data(), 1, size, o);
  fclose(o);
  fprintf(stderr, "%ux%u levels=%u fmt=%u bytesPerBlock=%u bytes=%u\n",
          w, h, levels, fmt, bpb, size);
  return 0;
}
