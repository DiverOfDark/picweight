package dev.picweight.android.di

import android.content.Context
import androidx.room.Room
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.PicweightDatabase
import javax.inject.Singleton

@Module
@InstallIn(SingletonComponent::class)
object DatabaseModule {

    @Provides
    @Singleton
    fun provideDatabase(@ApplicationContext context: Context): PicweightDatabase =
        Room.databaseBuilder(context, PicweightDatabase::class.java, PicweightDatabase.NAME)
            // Schema v1 has no predecessors, so this is unreachable today. It stops being
            // acceptable the moment a version bump ships while an upload could be queued:
            // a real migration is required then, because dropping the queue would drop a
            // logged meal, and G5 says that never happens.
            .fallbackToDestructiveMigration(dropAllTables = true)
            .build()

    @Provides
    @Singleton
    fun provideMealDao(database: PicweightDatabase): MealDao = database.mealDao()
}
