#import <CoreML/CoreML.h>

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// This bridge is limited to construction. Rust owns logical value conversion
// & copying; CoreML owns returned object & invokes deallocator exactly once.

static void hr_probe_message(void **out, NSString *message) {
    if (out && message) {
        *out = (__bridge_retained void *)message;
    }
}

static void *hr_probe_make_array(const int64_t *shape,
                                 size_t rank,
                                 MLMultiArrayDataType data_type,
                                 void **error_out,
                                 void **exception_out) {
    if (error_out) {
        *error_out = NULL;
    }
    if (exception_out) {
        *exception_out = NULL;
    }

    if (!shape || rank == 0 || rank > 64) {
        hr_probe_message(error_out, @"aligned probe: invalid rank or shape pointer");
        return NULL;
    }

    size_t *strides = calloc(rank, sizeof(*strides));
    if (!strides) {
        hr_probe_message(error_out, @"aligned probe: stride allocation failed");
        return NULL;
    }

    size_t storage_elements = 1;
    BOOL valid = YES;
    for (size_t axis = 0; axis < rank; axis++) {
        int64_t signed_dimension = shape[axis];
        if (signed_dimension <= 0) {
            valid = NO;
            break;
        }
    }
    if (valid) {
        size_t inner_dimension = (size_t)shape[rank - 1];
        if (inner_dimension > SIZE_MAX - 15) {
            valid = NO;
        } else {
            size_t rounded = (inner_dimension + 15) / 16;
            if (rounded > SIZE_MAX / 16) {
                valid = NO;
            } else {
                storage_elements = rounded * 16;
                strides[rank - 1] = 1;
                if (rank > 1) {
                    strides[rank - 2] = storage_elements;
                    for (size_t axis = rank - 2; axis > 0 && valid; axis--) {
                        size_t dimension = (size_t)shape[axis];
                        if (storage_elements > SIZE_MAX / dimension) {
                            valid = NO;
                            break;
                        }
                        storage_elements *= dimension;
                        strides[axis - 1] = storage_elements;
                    }
                }
                if (valid && rank > 1) {
                    size_t outer_dimension = (size_t)shape[0];
                    if (storage_elements > SIZE_MAX / outer_dimension) {
                        valid = NO;
                    } else {
                        storage_elements *= outer_dimension;
                    }
                }
            }
        }
    }
    if (!valid) {
        free(strides);
        hr_probe_message(error_out, @"aligned probe: shape or padded stride overflow");
        return NULL;
    }

    size_t element_size = 0;
    switch (data_type) {
        case MLMultiArrayDataTypeFloat16:
            element_size = sizeof(uint16_t);
            break;
        case MLMultiArrayDataTypeFloat32:
            element_size = sizeof(float);
            break;
        case MLMultiArrayDataTypeInt32:
            element_size = sizeof(int32_t);
            break;
        default:
            free(strides);
            hr_probe_message(error_out, @"aligned probe: unsupported data type");
            return NULL;
    }
    if (storage_elements > SIZE_MAX / element_size) {
        free(strides);
        hr_probe_message(error_out, @"aligned probe: byte size overflow");
        return NULL;
    }

    void *buffer = NULL;
    int allocation_status = posix_memalign(&buffer, 64, storage_elements * element_size);
    if (allocation_status != 0 || !buffer) {
        free(strides);
        hr_probe_message(error_out, @"aligned probe: posix_memalign failed");
        return NULL;
    }
    // Keep padded lanes deterministic in case backend reads complete tile.
    memset(buffer, 0, storage_elements * element_size);

    NSMutableArray<NSNumber *> *shape_values = [NSMutableArray arrayWithCapacity:rank];
    NSMutableArray<NSNumber *> *stride_values = [NSMutableArray arrayWithCapacity:rank];
    for (size_t axis = 0; axis < rank; axis++) {
        [shape_values addObject:@((NSInteger)shape[axis])];
        [stride_values addObject:@((NSInteger)strides[axis])];
    }
    free(strides);

    __block BOOL buffer_freed = NO;
    @try {
        NSError *error = nil;
        MLMultiArray *array = [[MLMultiArray alloc]
            initWithDataPointer:buffer
                          shape:shape_values
                       dataType:data_type
                        strides:stride_values
                    deallocator:^(void *bytes) {
                        if (!buffer_freed) {
                            buffer_freed = YES;
                            free(bytes);
                        }
                    }
                          error:&error];
        if (!array) {
            if (!buffer_freed) {
                buffer_freed = YES;
                free(buffer);
            }
            if (error) {
                hr_probe_message(error_out, error.localizedDescription);
            } else {
                hr_probe_message(error_out, @"aligned probe: CoreML returned no array");
            }
            return NULL;
        }
        return (__bridge_retained void *)array;
    } @catch (NSException *exception) {
        // If CoreML rejected after taking ownership, its block may already
        // have run; avoid freeing the same storage twice.
        if (!buffer_freed) {
            buffer_freed = YES;
            free(buffer);
        }
        NSString *reason = exception.reason ?: @"<no reason>";
        hr_probe_message(exception_out,
                         [NSString stringWithFormat:@"%@: %@", exception.name, reason]);
        return NULL;
    }
}

void *rightkit_dictation_coreml_aligned_array_f16(const int64_t *shape,
                                          size_t rank,
                                          void **error_out,
                                          void **exception_out) {
    return hr_probe_make_array(shape, rank, MLMultiArrayDataTypeFloat16,
                               error_out, exception_out);
}

void *rightkit_dictation_coreml_aligned_array_f32(const int64_t *shape,
                                          size_t rank,
                                          void **error_out,
                                          void **exception_out) {
    return hr_probe_make_array(shape, rank, MLMultiArrayDataTypeFloat32,
                               error_out, exception_out);
}

void *rightkit_dictation_coreml_aligned_array_i32(const int64_t *shape,
                                          size_t rank,
                                          void **error_out,
                                          void **exception_out) {
    return hr_probe_make_array(shape, rank, MLMultiArrayDataTypeInt32,
                               error_out, exception_out);
}
