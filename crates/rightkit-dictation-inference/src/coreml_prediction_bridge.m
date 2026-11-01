#import <CoreML/CoreML.h>

// Probe-only independent feature providers: each hypothesis owns its inputs.
// MLBatchProvider is an API batch; it does not imply a fused tensor graph.
void *rightkit_dictation_coreml_feature_batch(void *model_ptr, void *providers_ptr,
                                     const char **names, size_t name_count,
                                     void **error_out, void **exception_out) {
    *error_out = NULL;
    *exception_out = NULL;
    @try {
        MLModel *model = (__bridge MLModel *)model_ptr;
        NSArray *providers = (__bridge NSArray *)providers_ptr;
        MLArrayBatchProvider *batch = [[MLArrayBatchProvider alloc] initWithFeatureProviderArray:providers];
        NSError *error = nil;
        id<MLBatchProvider> result = [model predictionsFromBatch:batch error:&error];
        if (!result) {
            if (error) *error_out = (__bridge_retained void *)error;
            return NULL;
        }
        if ((NSUInteger)result.count != providers.count) {
            *exception_out = (__bridge_retained void *)@"feature batch count mismatch";
            return NULL;
        }
        NSMutableArray *outputs = [NSMutableArray arrayWithCapacity:result.count * name_count];
        for (NSInteger i = 0; i < result.count; i++) {
            id<MLFeatureProvider> features = [result featuresAtIndex:i];
            for (size_t j = 0; j < name_count; j++) {
                MLMultiArray *value = [[features featureValueForName:[NSString stringWithUTF8String:names[j]]] multiArrayValue];
                if (!value) {
                    *exception_out = (__bridge_retained void *)@"feature batch missing output";
                    return NULL;
                }
                [outputs addObject:value];
            }
        }
        return (__bridge_retained void *)outputs;
    } @catch (NSException *exception) {
        *exception_out = (__bridge_retained void *)[NSString stringWithFormat:@"%@: %@", exception.name, exception.reason];
        return NULL;
    }
}

// Keep Core ML's Objective-C exception entirely inside an Objective-C frame.
// Every non-null returned object is retained (+1) for Rust to adopt.
void *rightkit_dictation_coreml_prediction(void *model_ptr,
                                   void *provider_ptr,
                                   void **error_out,
                                   void **exception_description_out) {
    if (error_out) {
        *error_out = NULL;
    }
    if (exception_description_out) {
        *exception_description_out = NULL;
    }

    MLModel *model = (__bridge MLModel *)model_ptr;
    id<MLFeatureProvider> provider = (__bridge id<MLFeatureProvider>)provider_ptr;
    @try {
        NSError *error = nil;
        id<MLFeatureProvider> result = [model predictionFromFeatures:provider error:&error];
        if (result) {
            return (__bridge_retained void *)result;
        }
        if (error_out && error) {
            *error_out = (__bridge_retained void *)error;
        }
        return NULL;
    } @catch (NSException *exception) {
        if (exception_description_out) {
            NSString *reason = exception.reason ?: @"<no reason>";
            NSString *description = [NSString stringWithFormat:@"%@: %@", exception.name, reason];
            *exception_description_out = (__bridge_retained void *)description;
        }
        return NULL;
    }
}

// Scalar prediction variant used only by the opt-in output-backing probe.
// Every call is synchronous; batch prediction intentionally stays on the
// existing bridge path without MLPredictionOptions.
void *rightkit_dictation_coreml_prediction_with_output_backings(
    void *model_ptr,
    void *provider_ptr,
    const char **names,
    void **backings,
    size_t count,
    void **error_out,
    void **exception_description_out) {
    if (error_out) {
        *error_out = NULL;
    }
    if (exception_description_out) {
        *exception_description_out = NULL;
    }

    MLModel *model = (__bridge MLModel *)model_ptr;
    id<MLFeatureProvider> provider = (__bridge id<MLFeatureProvider>)provider_ptr;
    @try {
        NSMutableDictionary *outputBackings =
            [NSMutableDictionary dictionaryWithCapacity:count];
        for (size_t i = 0; i < count; i++) {
            NSString *name = [NSString stringWithUTF8String:names[i]];
            MLMultiArray *backing = (__bridge MLMultiArray *)backings[i];
            if (!name || !backing) {
                if (exception_description_out) {
                    *exception_description_out = (__bridge_retained void *)[
                        NSString stringWithFormat:@"%s", "invalid output backing probe entry"];
                }
                return NULL;
            }
            outputBackings[name] = backing;
        }

        MLPredictionOptions *options = [MLPredictionOptions new];
        options.outputBackings = outputBackings;
        NSError *error = nil;
        id<MLFeatureProvider> result =
            [model predictionFromFeatures:provider options:options error:&error];
        if (result) {
            return (__bridge_retained void *)result;
        }
        if (error_out && error) {
            *error_out = (__bridge_retained void *)error;
        }
        return NULL;
    } @catch (NSException *exception) {
        if (exception_description_out) {
            NSString *reason = exception.reason ?: @"<no reason>";
            NSString *description = [NSString stringWithFormat:@"%@: %@", exception.name, reason];
            *exception_description_out = (__bridge_retained void *)description;
        }
        return NULL;
    }
}

// Release-mode smoke for the Rust harness. This must return instead of letting
// an Objective-C exception cross the C ABI boundary.
void *rightkit_dictation_coreml_exception_smoke(void) {
    @try {
        @throw [NSException exceptionWithName:@"RightKitDictationCoreMLSmoke"
                                       reason:@"exception containment smoke"
                                     userInfo:nil];
    } @catch (NSException *exception) {
        NSString *description =
            [NSString stringWithFormat:@"%@: %@", exception.name, exception.reason];
        return (__bridge_retained void *)description;
    }
}

// Run a decoder state against several encoder frames as one CoreML batch.
// Every returned object is retained (+1) for Rust to adopt.
void *rightkit_dictation_coreml_joint_batch(void *model_ptr,
                                    void **encoder_ptrs,
                                    void *decoder_ptr,
                                    size_t count,
                                    const char *output_name,
                                    void **error_out,
                                    void **exception_description_out) {
    if (error_out) {
        *error_out = NULL;
    }
    if (exception_description_out) {
        *exception_description_out = NULL;
    }
    @try {
        MLModel *model = (__bridge MLModel *)model_ptr;
        MLMultiArray *decoder = (__bridge MLMultiArray *)decoder_ptr;
        NSMutableArray *providers = [NSMutableArray arrayWithCapacity:count];
        NSError *error = nil;
        for (size_t i = 0; i < count; i++) {
            MLMultiArray *encoder = (__bridge MLMultiArray *)encoder_ptrs[i];
            MLDictionaryFeatureProvider *provider = [[MLDictionaryFeatureProvider alloc]
                initWithDictionary:@{
                    @"encoder_step": encoder,
                    @"decoder_step": decoder,
                }
                error:&error];
            if (!provider) {
                if (error_out && error) {
                    *error_out = (__bridge_retained void *)error;
                }
                return NULL;
            }
            [providers addObject:provider];
        }
        MLArrayBatchProvider *batch = [[MLArrayBatchProvider alloc]
            initWithFeatureProviderArray:providers];
        id<MLBatchProvider> result = [model predictionsFromBatch:batch error:&error];
        if (!result) {
            if (error_out && error) {
                *error_out = (__bridge_retained void *)error;
            }
            return NULL;
        }
        if ((size_t)result.count != count) {
            if (exception_description_out) {
                *exception_description_out = (__bridge_retained void *)[
                    NSString stringWithFormat:@"batch returned %lu outputs for %lu inputs",
                                               (unsigned long)result.count,
                                               (unsigned long)count];
            }
            return NULL;
        }
        NSString *name = [NSString stringWithUTF8String:output_name];
        if (!name) {
            if (exception_description_out) {
                *exception_description_out = (__bridge_retained void *)[
                    NSString stringWithFormat:@"%s", "batch output name is invalid UTF-8"];
            }
            return NULL;
        }
        NSMutableArray *outputs = [NSMutableArray arrayWithCapacity:count];
        for (NSInteger i = 0; i < result.count; i++) {
            id<MLFeatureProvider> features = [result featuresAtIndex:i];
            MLMultiArray *array = [[features featureValueForName:name] multiArrayValue];
            if (!array) {
                if (exception_description_out) {
                    *exception_description_out = (__bridge_retained void *)[
                        NSString stringWithFormat:@"batch output %@ is missing or not a multiarray",
                                                   name];
                }
                return NULL;
            }
            [outputs addObject:array];
        }
        return (__bridge_retained void *)outputs;
    } @catch (NSException *exception) {
        if (exception_description_out) {
            NSString *reason = exception.reason ?: @"<no reason>";
            *exception_description_out = (__bridge_retained void *)[
                NSString stringWithFormat:@"%@ : %@", exception.name, reason];
        }
        return NULL;
    }
}
