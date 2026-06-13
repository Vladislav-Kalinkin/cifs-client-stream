#ifndef CIFS_CLIENT_STREAM_H
#define CIFS_CLIENT_STREAM_H

#ifdef __cplusplus
extern "C" {
#endif

char *cifs_client_stream_bridge_version(void);
void cifs_client_stream_free_string(char *ptr);

#ifdef __cplusplus
}
#endif

#endif
