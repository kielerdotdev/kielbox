/* Bindings input only. No C implementation, compiled bridge, or invented ABI. */
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avconfig.h>
#include <libavutil/buffer.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>

#if LIBAVCODEC_VERSION_INT != AV_VERSION_INT(61, 3, 100)
#error Expected the provider FFmpeg 7.0.2 libavcodec headers
#endif
#if LIBAVFORMAT_VERSION_INT != AV_VERSION_INT(61, 1, 100)
#error Expected the provider FFmpeg 7.0.2 libavformat headers
#endif
#if LIBAVUTIL_VERSION_INT != AV_VERSION_INT(59, 8, 100)
#error Expected the provider FFmpeg 7.0.2 libavutil headers
#endif
#if AV_HAVE_BIGENDIAN
#error This binding recipe targets little-endian Windows x86-64 only
#endif
