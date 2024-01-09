SELECT file_name, file_device, COUNT(*)
FROM bep_block_info
WHERE TRUE
GROUP BY file_device, file_name

/* SELECT f.*, bi.* */
/* FROM bep_file_info f */
/*     JOIN bep_block_info bi ON f.name = bi.file_name AND f.folder = bi.file_folder AND f.device = bi.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */

/* SELECT * */
/* FROM bep_file_info f */
/*     JOIN bep_file_version v ON f.name = v.file_name AND f.folder = v.file_folder AND f.device = v.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */
